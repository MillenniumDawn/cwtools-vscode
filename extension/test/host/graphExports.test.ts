import * as assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as vscode from "vscode";
import sinon from "sinon";
import {
	activate,
	graphPanelModule,
	EXTENSION_ID,
	waitUntil,
} from "../support/utils";
import type * as GraphPanelNamespace from "../../src/host/graphPanel";
import type { GraphData } from "../../src/common/graphTypes";

type GraphPanelModule = typeof GraphPanelNamespace;
type ExportPayload =
	| { command: "saveImage"; image: string }
	| { command: "saveJson"; json: string };
type ExportCommand = {
	command: "cwtools.saveGraphImage" | "cwtools.saveGraphJson";
	payloadCommand: ExportPayload["command"];
	fileName: string;
};

suite("Graph file exports", function () {
	this.timeout(60_000);

	const graphData: GraphData = [
		{
			id: "export-source",
			name: "Export Source",
			isPrimary: true,
			entityType: "test",
			location: { filename: "source.txt", line: 1, column: 0 },
			references: [{ key: "export-target", isOutgoing: true }],
		},
		{
			id: "export-target",
			name: "Export Target",
			isPrimary: false,
			entityType: "test",
			location: { filename: "target.txt", line: 2, column: 0 },
			references: [],
		},
	];
	const exportsToExercise: ExportCommand[] = [
		{
			command: "cwtools.saveGraphImage",
			payloadCommand: "saveImage",
			fileName: "graph.png",
		},
		{
			command: "cwtools.saveGraphJson",
			payloadCommand: "saveJson",
			fileName: "graph.json",
		},
	];

	let gp: GraphPanelModule;
	let outputDir: string;
	let panel: GraphPanelNamespace.GraphPanel;
	let sandbox: sinon.SinonSandbox;
	let pendingHandler: {
		command: ExportPayload["command"];
		resolve: (payload: ExportPayload) => void;
	} | undefined;

	setup(async () => {
		sandbox = sinon.createSandbox();
		gp = await graphPanelModule();
		const api = await activate();
		const extension = vscode.extensions.getExtension(EXTENSION_ID);
		assert.ok(api, "activated extension API is available");
		assert.ok(extension, "activated extension is available");
		gp.GraphPanel.currentPanel?.dispose();
		outputDir = fs.mkdtempSync(path.join(os.tmpdir(), "cwtools-graph-export-"));

		// Use a real VS Code webview panel. Wrap its actual registered host handler
		// so the test can await completion of its dialog and file-write work.
		const webviewPanel = vscode.window.createWebviewPanel(
			gp.GraphPanel.viewType,
			"Graph",
			vscode.ViewColumn.One,
			{ enableScripts: true, retainContextWhenHidden: true },
		);
		const register = webviewPanel.webview.onDidReceiveMessage.bind(
			webviewPanel.webview,
		);
		sandbox.stub(webviewPanel.webview, "onDidReceiveMessage").callsFake(
			(listener, thisArgs, disposables) =>
				register(async (value: ExportPayload) => {
					await listener.call(thisArgs, value);
					if (value.command === pendingHandler?.command) {
						const pending = pendingHandler;
						pendingHandler = undefined;
						pending.resolve(value);
					}
				}, thisArgs, disposables),
		);
		panel = gp.GraphPanel.restore(extension.extensionPath, webviewPanel);
		panel.initialiseGraph(graphData, 1);
		assert.ok(
			await waitUntil(() => panel.checkCytoscapeRendered(), 30_000),
			"the real graph webview rendered the graph before exporting",
		);
	});

	teardown(() => {
		pendingHandler = undefined;
		sandbox.restore();
		if (gp.GraphPanel.currentPanel) gp.GraphPanel.currentPanel.dispose();
		fs.rmSync(outputDir, { recursive: true, force: true });
	});

	function completedHostMessage(
		command: ExportPayload["command"],
	): Promise<ExportPayload> {
		return new Promise((resolve) => {
			pendingHandler = { command, resolve };
		});
	}

	for (const entry of exportsToExercise) {
		test(`${entry.command} writes a real file and safely handles dialog cancellation`, async () => {
			const api = await activate();
			assert.ok(api);
			const output = api.serverOutputChannel();
			assert.ok(output, "extension output channel is available");
			const outputErrors = sandbox.spy(output as vscode.LogOutputChannel, "error");
			const cancelled = completedHostMessage(entry.payloadCommand);
			sandbox.stub(vscode.window, "showSaveDialog").resolves(undefined);
			await vscode.commands.executeCommand(entry.command);
			await cancelled;
			assert.deepStrictEqual(fs.readdirSync(outputDir), [], "cancellation creates no file");
			assert.deepStrictEqual(
				outputErrors.getCalls().map((call) => String(call.args[0])).filter((message) =>
					message.includes("graph webview message handler failed"),
				),
				[],
				"canceled save completes without a logged handler error",
			);

			const destination = path.join(outputDir, entry.fileName);
			sandbox.restore();
			sandbox = sinon.createSandbox();
			const completed = completedHostMessage(entry.payloadCommand);
			sandbox.stub(vscode.window, "showSaveDialog").resolves(vscode.Uri.file(destination));
			await vscode.commands.executeCommand(entry.command);
			const payload = await completed;
			const bytes = await fs.promises.readFile(destination);
			assert.ok(bytes.length > 100, "export contains meaningful file data");

			if (payload.command === "saveImage") {
				const expectedBytes = Buffer.from(payload.image, "base64");
				assert.deepStrictEqual(bytes, expectedBytes, "saved image matches webview PNG bytes");
				assert.deepStrictEqual(
					bytes.subarray(0, 8),
					Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
				);
				assert.strictEqual(bytes.toString("ascii", 12, 16), "IHDR");
				assert.ok(bytes.readUInt32BE(16) > 0, "PNG image has a width");
				assert.ok(bytes.readUInt32BE(20) > 0, "PNG image has a height");
				assert.ok(bytes.includes(Buffer.from("IDAT")), "PNG contains image data chunks");
			} else {
				assert.strictEqual(bytes.toString("utf8"), payload.json);
				const exported = JSON.parse(payload.json) as {
					elements: {
						nodes: { data: { id: string; label?: string } }[];
						edges: { data: { source: string; target: string } }[];
					};
				};
				assert.deepStrictEqual(
					exported.elements.nodes.map(({ data }) => [data.id, data.label]).sort(),
					[["export-source", "Export Source"], ["export-target", "Export Target"]],
				);
				assert.deepStrictEqual(
					exported.elements.edges.map(({ data }) => [data.source, data.target]),
					[["export-source", "export-target"]],
				);
			}
		});
	}
});
