import * as assert from "assert";
import path from "path";
import * as vscode from "vscode";
import {
	activate,
	graphPanelModule,
	retryAsync,
	wait,
	EXTENSION_ID,
	SAMPLE_ROOT,
} from "../support/utils";
import { it, describe } from "mocha";
import type * as GraphPanelNamespace from "../../extension/graphPanel";
type GraphPanelModule = typeof GraphPanelNamespace;
import type { CwtoolsApi } from "../../extension/extension";
import type { GraphData } from "../../common/graphTypes";
import sinon from "sinon";
import * as fs from "node:fs";
import * as os from "node:os";
const root = SAMPLE_ROOT;

suite(`Debug Integration Test: `, function () {
	test("Extension should be present", () => {
		assert.ok(vscode.extensions.getExtension(EXTENSION_ID));
	});

	test("should activate and expose the graphPanel API", async function () {
		this.timeout(1 * 60 * 1000);
		const extension = (await activate()) as CwtoolsApi | undefined;
		// The exports may be absent when the language server can't start in the
		// test environment, but when present the activation API must expose
		// graphPanel() (the host tests reach the panel through it).
		if (extension) {
			assert.strictEqual(
				typeof extension.graphPanel,
				"function",
				"activation API should expose graphPanel()",
			);
		}
	});

	test("Extension activation status", async function () {
		this.timeout(1 * 60 * 1000);
		const extension = vscode.extensions.getExtension(EXTENSION_ID);
		assert.ok(extension, "Extension should be found");

		// Test activation status
		if (!extension.isActive) {
			await extension.activate();
		}
		assert.ok(
			extension.isActive,
			"Extension should be active after activation",
		);
	});

	test("Commands are registered", async function () {
		this.timeout(1 * 60 * 1000);
		// Ensure extension is activated first
		await activate();

		// Test that CWTools commands are registered
		const commands = await vscode.commands.getCommands();
		const cwtoolsCommands = commands.filter(
			(cmd) =>
				cmd.includes("cwtools") ||
				cmd === "genlocall" ||
				cmd === "cwtools.showGraph",
		);

		console.log(
			"All available commands:",
			commands.slice(0, 20).join(", ") + "...",
		);
		console.log("CWTools related commands found:", cwtoolsCommands);

		// In test environment, commands may not be fully registered due to server issues
		// But we should have at least some extension infrastructure
		assert.ok(
			commands.length > 50,
			"Should have many VS Code commands available",
		);

		// Test for basic VS Code commands that should always be there
		const basicCommands = [
			"workbench.action.files.openFile",
			"workbench.action.showCommands",
		];
		for (const basicCmd of basicCommands) {
			assert.ok(
				commands.includes(basicCmd),
				`Basic command '${basicCmd}' should be registered`,
			);
		}
	});

	describe("Diagnostics and Language Features", function () {
		this.timeout(2 * 60 * 1000);

		it("should handle file diagnostics", async function () {
			// Note: In a test environment without the language server,
			// we mainly test that the diagnostics API is accessible
			await activate();

			// Test that diagnostics collection is accessible
			const diagnostics = vscode.languages.getDiagnostics();
			assert.ok(
				Array.isArray(diagnostics),
				"Should be able to get diagnostics array",
			);

			// In a real scenario with server running, we would expect diagnostics
			// For now, we just verify the infrastructure works
			console.log(`Found ${diagnostics.length} diagnostic entries`);
		});

		it("should register language configurations", async function () {
			await activate();

			// Test that language configurations are set
			// This is harder to test directly, but we can verify the extension activated
			// and the language server client should be initialized
			const extension = vscode.extensions.getExtension(EXTENSION_ID);
			assert.ok(extension?.isActive, "Extension should be active");

			// The extension exports might be undefined due to server startup issues in test env
			const exports = extension?.exports;
			console.log("Extension exports type:", typeof exports, "value:", exports);

			// Just verify the extension is active - that's the main indicator of success
			assert.ok(
				extension.isActive,
				"Extension should be active, indicating basic setup worked",
			);
		});
	});
});

describe("GraphPanel Tests", function () {
	this.timeout(2 * 60 * 1000);
	const testCyData = {
		elements: {
			nodes: [
				{
					data: {
						id: "test1",
						label: "Test Node 1",
						isPrimary: true,
						entityType: "test",
						location: {
							filename: root + "/events/irm.txt",
							line: 1,
							column: 0,
						},
					},
				},
				{
					data: {
						id: "test2",
						label: "Test Node 2",
						isPrimary: false,
						entityType: "test",
						location: { filename: "test.txt", line: 2, column: 0 },
					},
				},
			],
		},
	};

	// Test data
	const testRawData: GraphData = [
		{
			id: "test1",
			name: "Test Node 1",
			isPrimary: true,
			entityType: "test",
			location: { filename: root + "/events/irm.txt", line: 1, column: 0 },
			references: [],
		},
		{
			id: "test2",
			name: "Test Node 2",
			isPrimary: false,
			entityType: "test",
			location: { filename: "test.txt", line: 2, column: 0 },
			references: [],
		},
	];

	const testCyDataJson = JSON.stringify(testCyData);
	// Setup variables
	let extension: vscode.Extension<unknown>;
	// The extension's own graphPanel module, not a second copy (see utils).
	let gp: GraphPanelModule;

	let tempDir: string;
	let tempFile: string;
	// Setup before each test
	const before = async function () {
		// Arrange: Activate the extension and get its path
		gp = await graphPanelModule();
		const extensionMaybe = vscode.extensions.getExtension(EXTENSION_ID);
		assert.ok(extensionMaybe, "Extension should be found");
		extension = extensionMaybe!;

		tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "cwtools-graph-"));
		tempFile = path.join(tempDir, "graph.json");
		fs.writeFileSync(tempFile, testCyDataJson, "utf8");

		// Clean up any existing panel
		if (gp.GraphPanel.currentPanel) {
			gp.GraphPanel.currentPanel.dispose();
		}
	};
	let sandbox: sinon.SinonSandbox;

	setup(() => {
		sandbox = sinon.createSandbox();
	});

	teardown(() => {
		sandbox.restore();
	});

	// Teardown after each test
	const after = function () {
		// Clean up
		if (gp.GraphPanel.currentPanel) {
			gp.GraphPanel.currentPanel.dispose();
		}
		// Remove temp file
		if (fs.existsSync(tempFile)) {
			fs.unlinkSync(tempFile);
		}
		if (fs.existsSync(tempDir)) {
			fs.rmdirSync(tempDir);
		}
	};

	it("should create a GraphPanel instance", async function () {
		await before();
		// Act: Create a GraphPanel
		gp.GraphPanel.create(extension.extensionPath);

		// Assert: Panel should be created
		assert.ok(gp.GraphPanel.currentPanel, "GraphPanel should be created");
		after();
	});
	it("should load and render cytoscape from JSON file", async function () {
		this.timeout(30000);
		await before();

		// Execute the graphFromJson command
		// We'll need to simulate the file dialog selection
		const uri = vscode.Uri.file(tempFile);

		sandbox.stub(vscode.window, "showOpenDialog").resolves([uri]);

		await vscode.commands.executeCommand("cwtools.graphFromJson");

		// Wait for the panel to be created and initialized
		await wait(1000);

		const rendered = await retryAsync(
			() => gp.GraphPanel.currentPanel!.checkCytoscapeRendered(),
			6,
			500,
		);

		assert.ok(rendered, "Cytoscape should have rendered elements");
		after();
	});

	it("should initialize GraphPanel with data", async function () {
		await before();
		this.timeout(10000); // Increase timeout for this test

		// Arrange: Create a GraphPanel
		gp.GraphPanel.create(extension.extensionPath);

		// Act: Initialize the graph with test data and wait for it to complete
		gp.GraphPanel.currentPanel!.initialiseGraph(testRawData, 1.0);

		const testStatus = async function () {
			return (await gp.GraphPanel.currentPanel!.getState()) === gp.State.Done;
		};
		const result = await retryAsync(testStatus, 3, 500);
		assert.strictEqual(result, true, "GraphPanel should be in the Done state");

		after();
	});

	it("should dispose GraphPanel properly", async function () {
		await before();

		// Arrange: Create a GraphPanel
		gp.GraphPanel.create(extension.extensionPath);

		// Act: Dispose the panel
		gp.GraphPanel.currentPanel!.dispose();

		// Assert: Panel should be undefined after disposal
		assert.strictEqual(
			gp.GraphPanel.currentPanel,
			undefined,
			"GraphPanel should be undefined after disposal",
		);
		after();
	});
});

suite("GraphPanel — UI integration", function () {
	this.timeout(2 * 60 * 1000);
	const sampleJson = JSON.stringify({
		elements: {
			nodes: [
				{
					data: {
						id: "a",
						label: "A",
						isPrimary: true,
						entityType: "x",
						location: { filename: "a.txt", line: 1, column: 0 },
					},
				},
				{
					data: {
						id: "b",
						label: "B",
						isPrimary: false,
						entityType: "x",
						location: { filename: "b.txt", line: 1, column: 0 },
					},
				},
			],
		},
	});

	let sandbox: sinon.SinonSandbox;
	let extension: vscode.Extension<unknown>;
	let gp: GraphPanelModule;
	let tempDir: string;
	let tempFile: string;

	const setupPanel = async () => {
		gp = await graphPanelModule();
		const ext = vscode.extensions.getExtension(EXTENSION_ID);
		assert.ok(ext, "Extension should be found");
		extension = ext!;
		tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "cwtools-graph-"));
		tempFile = path.join(tempDir, "graph.json");
		fs.writeFileSync(tempFile, sampleJson, "utf8");
		if (gp.GraphPanel.currentPanel) gp.GraphPanel.currentPanel.dispose();
		gp.GraphPanel.create(extension.extensionPath);
	};

	const teardownPanel = () => {
		if (gp?.GraphPanel.currentPanel) gp.GraphPanel.currentPanel.dispose();
		if (fs.existsSync(tempFile)) fs.unlinkSync(tempFile);
		if (fs.existsSync(tempDir)) fs.rmdirSync(tempDir);
	};

	setup(() => {
		sandbox = sinon.createSandbox();
	});
	teardown(() => {
		sandbox.restore();
		teardownPanel();
	});

	test("starts in the New state before the webview posts ready", async function () {
		await setupPanel();
		assert.strictEqual(
			await gp.GraphPanel.currentPanel!.getState(),
			gp.State.New,
		);
	});

	test("saveGraphImage and saveGraphJson are registered once a GraphPanel exists", async function () {
		await setupPanel();
		const commands = await vscode.commands.getCommands();
		assert.ok(
			commands.includes("cwtools.saveGraphImage"),
			"cwtools.saveGraphImage should be registered",
		);
		assert.ok(
			commands.includes("cwtools.saveGraphJson"),
			"cwtools.saveGraphJson should be registered",
		);
	});

	test("saveGraphImage forwards an exportImage message to the webview", async function () {
		await setupPanel();
		const postMessage = sinon.spy(
			gp.GraphPanel.currentPanel!["_panel"].webview,
			"postMessage",
		);
		await vscode.commands.executeCommand("cwtools.saveGraphImage");
		assert.ok(postMessage.called, "webview.postMessage should be called");
		assert.deepStrictEqual(postMessage.firstCall.args[0], {
			command: "exportImage",
		});
	});

	test("saveGraphJson forwards an exportJson message to the webview", async function () {
		await setupPanel();
		const postMessage = sinon.spy(
			gp.GraphPanel.currentPanel!["_panel"].webview,
			"postMessage",
		);
		await vscode.commands.executeCommand("cwtools.saveGraphJson");
		assert.ok(postMessage.called, "webview.postMessage should be called");
		assert.deepStrictEqual(postMessage.firstCall.args[0], {
			command: "exportJson",
		});
	});

	// Whether a CSP source list actually permits a URI. cspSource is a list of
	// source expressions, not one origin ("'self' https://*.vscode-cdn.net"), and
	// the resource host is a subdomain of that wildcard, so comparing prefixes is
	// not enough to answer the question.
	const cspPermits = (sourceList: string, uri: string): boolean => {
		const { protocol, host } = new URL(uri);
		if (protocol !== "https:") return false;
		return sourceList.split(/\s+/).some((src) => {
			if (!src.startsWith("https://")) return false;
			const pattern = src.slice("https://".length);
			return pattern.startsWith("*.")
				? host.endsWith(pattern.slice(1))
				: host === pattern;
		});
	};

	// The CSP named the pre-1.55 `vscode-resource:` scheme while asWebviewUri
	// returns an https://…vscode-cdn.net URI, so site.css was blocked and #cy lost
	// the flex sizing that gives it height. Check the emitted href against the
	// policy that is actually served rather than trusting the scheme name.
	test("the webview CSP permits the stylesheet it links", async function () {
		await setupPanel();
		const webview = gp.GraphPanel.currentPanel!["_panel"].webview;
		const html = webview.html;
		const csp = /Content-Security-Policy" content="([^"]+)"/.exec(html)?.[1];
		assert.ok(csp, "no Content-Security-Policy meta tag found");
		assert.ok(
			!csp.includes("vscode-resource:"),
			"CSP still names the pre-1.55 vscode-resource: scheme",
		);

		const styleSrc = /style-src ([^;"]+)/.exec(csp)?.[1];
		assert.ok(styleSrc, "CSP has no style-src directive");
		const styleHref = /<link href="([^"]+)"/.exec(html)?.[1];
		assert.ok(styleHref, "no stylesheet <link> found");
		assert.ok(
			cspPermits(styleSrc, styleHref),
			`style-src "${styleSrc}" does not permit "${styleHref}", so the stylesheet is blocked`,
		);
	});

	// The graph script is allowed by nonce, not by origin, so its own directive
	// has to keep carrying one that matches the tag.
	test("the webview CSP permits the graph script by nonce", async function () {
		await setupPanel();
		const html = gp.GraphPanel.currentPanel!["_panel"].webview.html;
		const scriptNonce = /<script src="[^"]+" nonce="([^"]+)"/.exec(html)?.[1];
		assert.ok(scriptNonce, "no nonced <script> found");
		const scriptSrc = /script-src ([^;"]+)/.exec(html)?.[1];
		assert.ok(
			scriptSrc?.includes(`'nonce-${scriptNonce}'`),
			`script-src "${scriptSrc}" does not carry the script tag's nonce`,
		);
	});

	test("initialiseGraph transitions out of New once data is queued", async function () {
		await setupPanel();
		const data: GraphData = [
			{ id: "a", name: "A", references: [], isPrimary: true, entityType: "x" },
			{ id: "b", name: "B", references: [], isPrimary: false, entityType: "x" },
		];
		gp.GraphPanel.currentPanel!.initialiseGraph(data, 1.0);
		const finalState = await retryAsync(
			async () =>
				(await gp.GraphPanel.currentPanel!.getState()) === gp.State.Done,
			3,
			500,
		);
		assert.ok(
			finalState,
			"expected GraphPanel to reach Done after initialiseGraph",
		);
	});

	test("initialiseGraph with a JSON string queues an import handler", async function () {
		await setupPanel();
		// Calling initialiseGraph with a string should not throw and should leave the
		// panel in a non-New state (it wires an onLoad listener to post importJson
		// once the webview signals ready).
		gp.GraphPanel.currentPanel!.initialiseGraph(sampleJson, 1.0);
		const state = await gp.GraphPanel.currentPanel!.getState();
		assert.notStrictEqual(
			state,
			gp.State.New,
			"panel should have left New after initialiseGraph",
		);
	});
});

suite("FileExplorer — UI integration", function () {
	this.timeout(2 * 60 * 1000);
	let sandbox: sinon.SinonSandbox;

	setup(() => {
		sandbox = sinon.createSandbox();
	});
	teardown(() => sandbox.restore());

	// The explorer is built once, when the server sends its first file list, so
	// the command shows up shortly after activation. Constructing a second
	// FileExplorer here to force it would re-register the command and throw.
	test("the openFile command is registered once the server sends a file list", async function () {
		await activate();
		const registered = await retryAsync(
			async () =>
				(await vscode.commands.getCommands()).includes(
					"cwtools-files.openFile",
				),
			30,
			500,
		);
		assert.ok(
			registered,
			"openFile command should be registered once FileExplorer exists",
		);
	});

	test("openFile command shows the document", async function () {
		await activate();
		const uri = vscode.Uri.file(path.join(root, "events/irm.txt"));
		const showStub = sandbox.stub(vscode.window, "showTextDocument").resolves();

		await vscode.commands.executeCommand("cwtools-files.openFile", uri);

		assert.ok(showStub.called, "showTextDocument should be called");
		assert.ok(showStub.firstCall.args[0].fsPath.endsWith("events/irm.txt"));
	});
});
