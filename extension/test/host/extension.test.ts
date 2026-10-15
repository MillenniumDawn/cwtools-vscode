import * as assert from "assert";
import path from "path";
import * as vscode from "vscode";
import {
	activate,
	graphPanelModule,
	waitForServerReady,
	waitUntil,
	EXTENSION_ID,
	SAMPLE_ROOT,
} from "../support/utils";
import type * as GraphPanelNamespace from "../../src/host/graphPanel";
type GraphPanelModule = typeof GraphPanelNamespace;
import type { GraphData } from "../../src/common/graphTypes";
import type { ElementsDefinition } from "cytoscape";
import sinon from "sinon";
import * as fs from "node:fs";
import * as os from "node:os";
const root = SAMPLE_ROOT;

// The pinned Stellaris config's summary (116 errors, first CW605) can arrive
// after readiness while an error stub is active. It is unrelated to a command's
// result, so the command checks below filter it out.
const isRulesConfigSummaryNotification = (message: string): boolean =>
	/^CWTools: \d+ rules-config error\(s\), first:/.test(message);

suite("Activation", function () {
	this.timeout(2 * 60 * 1000);

	test("activates and exposes the host-test API", async function () {
		const api = await activate();
		assert.ok(api, "activation API should be exposed");
		assert.strictEqual(vscode.extensions.getExtension(EXTENSION_ID)?.isActive, true);
		assert.strictEqual(typeof api.graphPanel, "function");
		assert.strictEqual(typeof api.serverOutputChannel, "function");
	});

	test("registers the CWTools commands", async function () {
		await activate();
		const commands = await vscode.commands.getCommands();
		const required = [
			"cwtools.showReferences",
			"cwtools.showOutput",
			"cwtools.restartServer",
			"cwtools.showGraph",
			"cwtools.setGraphDepth",
			"cwtools.graphFromJson",
			"cwtools.exportProfilingLog",
			"cwtools.fixAllWorkspace",
			"cwtools.formatWorkspace",
			"cwtools.statusBarMenu",
			"genlocall",
		];
		assert.deepStrictEqual(
			required.filter((command) => !commands.includes(command)),
			[],
			"CWTools commands should be registered",
		);
	});

	test("the status item settles on the ready text once the initial scan finishes", async function () {
		const api = await activate();
		assert.ok(api, "activation API should be exposed");
		assert.ok(
			await waitForServerReady(api),
			`status item should settle to ready, last seen: ${api.serverStatusText()}`,
		);
	});

	test("treats dotted Paradox identifiers as one word", async function () {
		await activate();
		const document = await vscode.workspace.openTextDocument(
			path.join(root, "events", "irm.txt"),
		);
		const range = document.getWordRangeAtPosition(new vscode.Position(8, 9));

		assert.strictEqual(document.languageId, "paradox");
		assert.ok(range, "dotted identifier should have a word range");
		assert.strictEqual(document.getText(range), "irm.1");
	});

	// The gate reads the server's advertised commands; both branches are
	// unit-tested in graphAvailability.test.ts. This pins which branch the
	// bundled engine takes, so a pin that loses the command fails here rather
	// than reaching users as a raw protocol error.
	for (const [command, unsupported] of [
		["fixAllWorkspace", "doesn't support fixing the workspace"],
		["formatWorkspace", "doesn't support formatting the workspace"],
	]) {
		test(`${command} runs against the pinned server without a protocol error`, async function () {
			const api = await activate();
			assert.ok(api, "activation API should be exposed");
			const advertised = api.serverCommands();
			assert.ok(
				advertised.includes(command),
				`pinned server should advertise ${command}, got: ${advertised.join(", ")}`,
			);

			// activate() returns while the server is still indexing, and a
			// command sent into that window comes back as a transport rejection
			// rather than a result (#675). Gate on the scan the way every other
			// suite that talks to the server does.
			assert.ok(
				await waitForServerReady(api),
				`server never finished its initial scan, last status: ${api.serverStatusText()}`,
			);

			const sandbox = sinon.createSandbox();
			const warn = sandbox.stub(vscode.window, "showWarningMessage");
			const err = sandbox.stub(vscode.window, "showErrorMessage");
			try {
				await vscode.commands.executeCommand(`cwtools.${command}`);
			} finally {
				sandbox.restore();
			}
			const warnings = warn.getCalls().map((c) => String(c.args[0]));
			assert.deepStrictEqual(
				warnings.filter((m) => m.includes(unsupported)),
				[],
				"unexpected upgrade warning",
			);
			const errors = err.getCalls().map((c) => String(c.args[0]));
			assert.deepStrictEqual(
				errors.filter((message) => !isRulesConfigSummaryNotification(message)),
				[],
				`no raw protocol error should be surfaced (status: ${api.serverStatusText()})`,
			);
			// A crashed server is restarted silently now (#675), so nothing else
			// here would notice one. initializeResult is cleared while the client
			// is not Running, which makes the advertised list the crash probe.
			assert.ok(
				api.serverCommands().includes(command),
				"the client left Running during the command; the server crashed and restarted",
			);
		});
	}
});

suite("GraphPanel", function () {
	this.timeout(2 * 60 * 1000);
	const graphJson = JSON.stringify({
		elements: {
			nodes: [
				{
					data: {
						id: "test1",
						label: "Test Node 1",
						isPrimary: true,
						entityType: "test",
						location: { filename: root + "/events/irm.txt", line: 1, column: 0 },
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
	});
	const graphData: GraphData = [
		{
			id: "test1",
			name: "Test Node 1",
			isPrimary: true,
			entityType: "test",
			location: { filename: root + "/events/irm.txt", line: 1, column: 0 },
			references: [{ key: "test2", isOutgoing: true }],
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

	let extension: vscode.Extension<unknown>;
	// The extension's own graphPanel module, not a second copy (see utils).
	let gp: GraphPanelModule;
	let tempDir: string;
	let tempFile: string;
	let sandbox: sinon.SinonSandbox;

	setup(async () => {
		sandbox = sinon.createSandbox();
		gp = await graphPanelModule();
		const found = vscode.extensions.getExtension(EXTENSION_ID);
		assert.ok(found, "Extension should be found");
		extension = found;

		tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "cwtools-graph-"));
		tempFile = path.join(tempDir, "graph.json");
		fs.writeFileSync(tempFile, graphJson, "utf8");
		gp.GraphPanel.currentPanel?.dispose();
	});

	// Closing a panel whose webview is still loading kills the ready handshake
	// mid-flight, which is the window #216's silent host aborts point at.
	teardown(async () => {
		sandbox.restore();
		if (gp.GraphPanel.currentPanel) {
			await waitUntil(
				() => gp.GraphPanel.currentPanel?.getState() !== gp.State.New,
				2_000,
			);
			gp.GraphPanel.currentPanel?.dispose();
		}
		if (
			vscode.window.activeTextEditor?.document.uri.fsPath ===
			vscode.Uri.file(tempFile).fsPath
		) {
			await vscode.commands.executeCommand("workbench.action.closeActiveEditor");
		}
		fs.rmSync(tempDir, { recursive: true, force: true });
	});

	function createPanel(): GraphPanelNamespace.GraphPanel {
		gp.GraphPanel.create(extension.extensionPath);
		assert.ok(gp.GraphPanel.currentPanel, "GraphPanel should be created");
		return gp.GraphPanel.currentPanel;
	}

	const importSelectedFile = () => {
		sandbox.stub(vscode.window, "showOpenDialog").resolves([
			vscode.Uri.file(tempFile),
		]);
		return vscode.commands.executeCommand("cwtools.graphFromJson");
	};

	async function waitForRenderedGraph(panel: GraphPanelNamespace.GraphPanel) {
		assert.ok(
			await waitUntil(() => panel.getState() === gp.State.Done),
			"GraphPanel should reach the Done state",
		);
		assert.ok(
			await waitUntil(() => panel.checkCytoscapeRendered()),
			"Cytoscape should render the supplied graph",
		);
	}

	type SaveCommand = "cwtools.saveGraphImage" | "cwtools.saveGraphJson";
	type SaveMessage = "saveImage" | "saveJson";
	const saves: readonly (readonly [SaveCommand, SaveMessage])[] = [
		["cwtools.saveGraphImage", "saveImage"],
		["cwtools.saveGraphJson", "saveJson"],
	];

	// Awaits the host's handler, not just the message, so the disk is settled before it is read.
	async function runSave(
		panel: GraphPanelNamespace.GraphPanel,
		command: SaveCommand,
		message: SaveMessage,
		dest: string | undefined,
	) {
		const handler = sandbox.spy(
			panel as unknown as {
				handleWebviewMessage: (message: { command: string }) => Promise<void>;
			},
			"handleWebviewMessage",
		);
		const dialog = sandbox
			.stub(vscode.window, "showSaveDialog")
			.resolves(dest === undefined ? undefined : vscode.Uri.file(dest));
		await vscode.commands.executeCommand(command);
		const handled = () =>
			handler.getCalls().find((call) => call.args[0].command === message);
		assert.ok(
			await waitUntil(() => handled() !== undefined, 30_000),
			`${message} should reach the host`,
		);
		await handled()!.returnValue;
		assert.strictEqual(dialog.callCount, 1);
	}

	async function assertRenderedGraph(panel: GraphPanelNamespace.GraphPanel) {
		await waitForRenderedGraph(panel);
		const dest = path.join(tempDir, "export.json");
		await runSave(panel, "cwtools.saveGraphJson", "saveJson", dest);
		const graph = JSON.parse(fs.readFileSync(dest, "utf8")) as {
			elements: ElementsDefinition;
		};
		assert.deepStrictEqual(
			graph.elements.nodes.map((node) => node.data.id).sort(),
			["test1", "test2"],
		);
		assert.deepStrictEqual(
			graph.elements.edges.map((edge) => [edge.data.source, edge.data.target]),
			[["test1", "test2"]],
		);
	}

	// A rebuilt panel would duplicate the webview and the save commands it
	// registers.
	test("reuses the current panel when create is called twice and clears it on dispose", function () {
		const first = createPanel();
		const createWebviewPanel = sandbox.spy(vscode.window, "createWebviewPanel");

		assert.strictEqual(createPanel(), first);
		assert.strictEqual(createWebviewPanel.callCount, 0);

		first.dispose();
		assert.strictEqual(gp.GraphPanel.currentPanel, undefined);
	});

	test("loads and renders a graph from a JSON file", async function () {
		await importSelectedFile();

		assert.ok(
			await waitUntil(() => gp.GraphPanel.currentPanel!.checkCytoscapeRendered()),
			"Cytoscape should have rendered elements",
		);
	});

	test("renders server graph data and cwtools.saveGraphJson writes the nodes and edges it rendered", async function () {
		const panel = createPanel();

		panel.initialiseGraph(graphData, 1.0);

		await assertRenderedGraph(panel);
	});

	test("cwtools.saveGraphImage writes a PNG of the rendered graph", async function () {
		const panel = createPanel();
		panel.initialiseGraph(graphData, 1.0);
		await waitForRenderedGraph(panel);
		const dest = path.join(tempDir, "export.png");

		await runSave(panel, "cwtools.saveGraphImage", "saveImage", dest);

		const png = fs.readFileSync(dest);
		assert.deepStrictEqual(
			png.subarray(0, 8),
			Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
		);
		assert.strictEqual(png.subarray(12, 16).toString("ascii"), "IHDR");
		assert.ok(png.readUInt32BE(16) > 0 && png.readUInt32BE(20) > 0);
	});

	for (const [command, message] of saves) {
		test(`${command} writes nothing when the save dialog is cancelled`, async function () {
			const panel = createPanel();
			panel.initialiseGraph(graphData, 1.0);
			await waitForRenderedGraph(panel);

			await runSave(panel, command, message, undefined);

			assert.deepStrictEqual(fs.readdirSync(tempDir), ["graph.json"]);
		});
	}

	// The window-reload serializer hands back a live panel whose html the
	// extension must re-set; restore() adopts it like a fresh panel.
	test("restores a GraphPanel around a revived webview panel", async function () {
		const panel = vscode.window.createWebviewPanel(
			"cwtools-graph",
			"Graph",
			vscode.ViewColumn.One,
			{ enableScripts: true },
		);
		const revived = gp.GraphPanel.restore(extension.extensionPath, panel);
		assert.strictEqual(gp.GraphPanel.currentPanel, revived);

		revived.initialiseGraph(graphData, 1.0, {
			source: "server",
			entityType: "test",
			depth: 3,
		});

		await assertRenderedGraph(revived);
	});

	for (const [name, json] of [
		["malformed JSON", "{broken"],
		["elements without IDs", '{"elements":[{"data":{}}]}'],
	]) {
		test(`reports ${name} from the real graph import command`, async function () {
			createPanel();
			fs.writeFileSync(tempFile, json, "utf8");
			const error = sandbox.stub(vscode.window, "showErrorMessage").resolves();

			await importSelectedFile();

			assert.ok(await waitUntil(() => error.called, 30_000));
			assert.deepStrictEqual(
				error.getCalls().map((call) => call.args[0]),
				['CWTools: couldn\'t import "graph.json": it isn\'t valid Cytoscape graph JSON.'],
			);
			assert.strictEqual(
				await gp.GraphPanel.currentPanel!.checkCytoscapeRendered(),
				false,
			);
		});
	}

	for (const [command, kind] of [
		["cwtools.saveGraphImage", "an image"],
		["cwtools.saveGraphJson", "a JSON file"],
	]) {
		test(`${command} reports an unavailable graph through the webview`, async function () {
			const panel = createPanel();
			const error = sandbox.stub(vscode.window, "showErrorMessage").resolves();
			const save = sandbox.stub(vscode.window, "showSaveDialog").resolves();
			assert.ok(
				await waitUntil(() => panel.getState() === gp.State.ClientReady, 30_000),
			);

			await vscode.commands.executeCommand(command);

			assert.ok(await waitUntil(() => error.called, 30_000));
			assert.deepStrictEqual(
				error.getCalls().map((call) => call.args[0]),
				[`CWTools: no graph is available to export as ${kind}.`],
			);
			assert.strictEqual(save.called, false);
		});
	}

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
	// returns an https://…vscode-cdn.net URI, so the webview styles were blocked
	// and #cy lost the flex sizing that gives it height. The graph script is
	// allowed by nonce, not by origin, so its directive has to keep carrying one
	// that matches the tag.
	test("the webview CSP permits its stylesheets and script, and the graph still renders", async function () {
		const panel = createPanel();
		const webview = panel["_panel"].webview;
		const html = webview.html;
		const csp = /Content-Security-Policy" content="([^"]+)"/.exec(html)?.[1];
		assert.ok(csp, "no Content-Security-Policy meta tag found");
		const directive = (name: string): string => {
			const sources = new RegExp(`${name} ([^;"]+)`).exec(csp)?.[1];
			assert.ok(sources, `CSP has no ${name} directive`);
			return sources.trim();
		};

		const styleSrc = directive("style-src");
		const styleHrefs = [...html.matchAll(/<link href="([^"]+)"/g)].map(
			(match) => match[1],
		);
		assert.deepStrictEqual(
			styleHrefs.map((href) => path.posix.basename(new URL(href).pathname)).sort(),
			["graph.css", "site.css"],
		);
		for (const styleHref of styleHrefs) {
			assert.ok(
				cspPermits(styleSrc, styleHref),
				`style-src "${styleSrc}" does not permit "${styleHref}", so the stylesheet is blocked`,
			);
		}

		const imgSrc = directive("img-src").split(/\s+/);
		assert.ok(imgSrc.join(" ").includes(webview.cspSource));
		assert.ok(imgSrc.includes("data:"), "img-src must preserve data URLs");
		assert.ok(!imgSrc.includes("https:"), "img-src must not allow every HTTPS origin");
		assert.strictEqual(directive("form-action"), "'none'");

		const scriptNonce = /<script src="[^"]+" nonce="([^"]+)"/.exec(html)?.[1];
		assert.ok(scriptNonce, "no nonced <script> found");
		assert.ok(directive("script-src").includes(`'nonce-${scriptNonce}'`));

		panel.initialiseGraph(graphData, 1.0);
		assert.ok(
			await waitUntil(() => panel.checkCytoscapeRendered(), 30_000),
			"graph content should render under the CSP",
		);
	});

	// The host cannot script the webview page, so these post its message by hand.
	suite("goToFile", function () {
		let panel: GraphPanelNamespace.GraphPanel;
		let show: sinon.SinonStub;
		let revealRange: sinon.SinonStub;
		let warn: sinon.SinonStub;
		let receive: Parameters<vscode.Webview["onDidReceiveMessage"]>[0] | undefined;
		let originalShow: typeof vscode.window.showTextDocument;
		let actualReveal: sinon.SinonSpy;

		setup(() => {
			receive = undefined;
			const createWebviewPanelOriginal = vscode.window.createWebviewPanel.bind(
				vscode.window,
			);
			sandbox
				.stub(vscode.window, "createWebviewPanel")
				.callsFake((...args) => {
					const created = createWebviewPanelOriginal(...args);
					const webview = created.webview;
					const register = webview.onDidReceiveMessage.bind(webview);
					sandbox
						.stub(webview, "onDidReceiveMessage")
						.callsFake((listener, thisArgs, disposables) => {
							receive = listener;
							return register(listener, thisArgs, disposables);
						});
					return created;
				});
			originalShow = vscode.window.showTextDocument.bind(vscode.window);
			panel = createPanel();
			revealRange = sandbox.stub();
			show = sandbox
				.stub(vscode.window, "showTextDocument")
				.resolves({ revealRange } as unknown as vscode.TextEditor);
			warn = sandbox.stub(vscode.window, "showWarningMessage");
		});

		const openedPath = () => (show.firstCall.args[0] as vscode.Uri).fsPath;
		const goToFile = (uri: string, line = 1, column = 1) =>
			panel["handleWebviewMessage"]({ command: "goToFile", uri, line, column });

		test("prompts for a path outside every trusted root and does not open it when declined", async function () {
			warn.resolves(undefined);

			await goToFile(tempFile);

			assert.strictEqual(warn.callCount, 1);
			assert.ok(
				String(warn.firstCall.args[0]).includes(vscode.Uri.file(tempFile).fsPath),
				`prompt should name the path, got: ${String(warn.firstCall.args[0])}`,
			);
			assert.deepStrictEqual(warn.firstCall.args[1], { modal: true });
			assert.strictEqual(show.called, false);
		});

		test("opens a path outside every trusted root once the prompt is accepted", async function () {
			warn.callsFake(
				(_message: string, _options: vscode.MessageOptions, ...items: string[]) =>
					Promise.resolve(items[0]),
			);

			await goToFile(tempFile);

			assert.strictEqual(warn.callCount, 1);
			assert.strictEqual(show.callCount, 1);
			assert.strictEqual(openedPath(), vscode.Uri.file(tempFile).fsPath);
		});

		test("opens through the registered listener and clamps zero coordinates", async function () {
			warn.callsFake(
				(_message: string, _options: vscode.MessageOptions, ...items: string[]) =>
					Promise.resolve(items[0]),
			);
			show.callsFake(async () => {
				const editor = await originalShow(vscode.Uri.file(tempFile));
				actualReveal = sandbox.spy(editor, "revealRange");
				return editor;
			});
			assert.ok(receive, "GraphPanel should register its webview message listener");

			await receive({ command: "goToFile", uri: tempFile, line: 0, column: 0 });

			const editor = vscode.window.activeTextEditor;
			assert.ok(editor, "the file should open in a real editor");
			assert.strictEqual(editor.document.uri.fsPath, vscode.Uri.file(tempFile).fsPath);
			assert.strictEqual(show.callCount, 1);
			const range = actualReveal.firstCall.args[0] as vscode.Range;
			assert.deepStrictEqual(range.start, new vscode.Position(0, 0));
			assert.strictEqual(warn.callCount, 1);
		});

		test("opens a workspace file without a prompt at the 0-based line and column", async function () {
			const inside = path.join(root, "events", "irm.txt");

			await goToFile(inside, 5, 3);

			assert.strictEqual(warn.called, false);
			assert.strictEqual(show.callCount, 1);
			assert.strictEqual(openedPath(), vscode.Uri.file(inside).fsPath);
			assert.strictEqual(revealRange.callCount, 1);
			const [range, revealType] = revealRange.firstCall.args as [
				vscode.Range,
				vscode.TextEditorRevealType,
			];
			assert.ok(
				range.isEqual(new vscode.Range(4, 2, 4, 2)),
				`expected 4:2, got ${range.start.line}:${range.start.character}`,
			);
			assert.strictEqual(revealType, vscode.TextEditorRevealType.AtTop);
		});

		test("refuses a relative path without a prompt", async function () {
			// Uri.file roots a relative string, so only the handler's own check refuses it.
			await goToFile(path.join("events", "irm.txt"));

			assert.strictEqual(warn.called, false);
			assert.strictEqual(show.called, false);
		});
	});
});

suite("FileExplorer", function () {
	this.timeout(2 * 60 * 1000);
	const uri = vscode.Uri.file(path.join(root, "events/irm.txt"));
	let sandbox: sinon.SinonSandbox;

	// The explorer is built once, when the server sends its first file list, so
	// the command shows up shortly after activation. Constructing a second
	// FileExplorer here to force it would re-register the command and throw.
	setup(async () => {
		sandbox = sinon.createSandbox();
		await activate();
		assert.ok(
			await waitUntil(async () =>
				(await vscode.commands.getCommands()).includes("cwtools-files.openFile"),
			),
			"openFile command should be registered once FileExplorer exists",
		);
	});
	teardown(() => sandbox.restore());

	test("openFile command shows the document", async function () {
		const showStub = sandbox.stub(vscode.window, "showTextDocument").resolves();

		await vscode.commands.executeCommand("cwtools-files.openFile", uri);

		assert.ok(showStub.called, "showTextDocument should be called");
		assert.ok(showStub.firstCall.args[0].fsPath.endsWith("events/irm.txt"));
	});

	test("openFile command shows an error when the document cannot be opened", async function () {
		sandbox.stub(vscode.window, "showTextDocument").rejects(new Error("gone"));
		const errStub = sandbox.stub(vscode.window, "showErrorMessage").resolves();

		await vscode.commands.executeCommand("cwtools-files.openFile", uri);

		assert.ok(errStub.called, "showErrorMessage should be called");
		assert.ok(
			String(errStub.firstCall.args[0]).includes(uri.fsPath),
			`error should name the path, got: ${String(errStub.firstCall.args[0])}`,
		);
	});
});
