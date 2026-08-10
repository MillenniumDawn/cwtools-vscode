import * as vscode from "vscode";
import type { ExtensionContext } from "vscode";
import { workspace, window, commands } from "vscode";
import { ExecuteCommandRequest } from "vscode-languageclient/node";
import type { LanguageClient } from "vscode-languageclient/node";
import { getGraphData, type GraphPanelState } from "../common/graphTypes";
import {
	graphDataAvailable,
	fixAllWorkspaceAvailable,
} from "./graphAvailability";
import type { EditorTracker } from "./documentLanguage";
import { errorMessage, logError } from "./logger";

function serverProvidesGraphData(client: LanguageClient): boolean {
	return graphDataAvailable(
		client.initializeResult?.capabilities.executeCommandProvider?.commands,
	);
}

function serverProvidesFixAll(client: LanguageClient): boolean {
	return fixAllWorkspaceAvailable(
		client.initializeResult?.capabilities.executeCommandProvider?.commands,
	);
}

// Gates the palette entries and the editor-title button; call once the server
// has started and its capabilities are known.
export function publishCommandAvailability(client: LanguageClient): void {
	void commands.executeCommand(
		"setContext",
		"cwtoolsGraphAvailable",
		serverProvidesGraphData(client),
	);
	void commands.executeCommand(
		"setContext",
		"cwtoolsFixAllAvailable",
		serverProvidesFixAll(client),
	);
}

export function registerCommands(
	context: ExtensionContext,
	client: LanguageClient,
	tracker: EditorTracker,
): void {
	let currentGraphDepth = 3;
	const wheelSensitivity = (): number =>
		workspace.getConfiguration("cwtools.graph").get("zoomSensitivity") ?? 1;
	const showGraph = async function () {
		if (!serverProvidesGraphData(client)) {
			window.showWarningMessage(
				"CWTools: this language server doesn't provide graph data, so a graph can only be opened from a saved export. " +
					"Run 'cwtools: Recreate graph from json'.",
			);
			return;
		}
		const entityType = tracker.getLatestType();
		const [gp, graphData] = await Promise.all([
			import("./graphPanel"),
			getGraphData(entityType, currentGraphDepth),
		]);
		gp.GraphPanel.create(context.extensionPath);
		gp.GraphPanel.currentPanel!.initialiseGraph(graphData, wheelSensitivity(), {
			source: "server",
			entityType,
			depth: currentGraphDepth,
		});
	};
	context.subscriptions.push(
		commands.registerCommand("cwtools.showGraph", async () => {
			await showGraph();
		}),
	);
	context.subscriptions.push(
		commands.registerCommand("cwtools.setGraphDepth", async () => {
			// Redrawing at a new depth re-queries the server, so a graph imported
			// from JSON can't be re-cut without it either.
			if (!serverProvidesGraphData(client)) {
				window.showWarningMessage(
					"CWTools: this language server doesn't provide graph data, so the graph depth can't be changed.",
				);
				return;
			}
			const res = await window.showInputBox({
				placeHolder: "default: 3",
				prompt:
					"Set graph depth (how many connections to go back from this file)",
				value: currentGraphDepth.toString(),
				validateInput: (v: string) =>
					Number.isInteger(Number(v)) ? undefined : "Please enter a number",
			});
			if (Number.isInteger(Number(res))) {
				currentGraphDepth = Number(res);
				await showGraph();
			}
		}),
	);
	context.subscriptions.push(
		commands.registerCommand("cwtools.graphFromJson", async () => {
			const uri = await window.showOpenDialog({ filters: { Json: ["json"] } });
			if (!uri) {
				return;
			}
			const bytes = await vscode.workspace.fs.readFile(uri[0]);
			const data = new TextDecoder("utf-8").decode(bytes);
			const gp = await import("./graphPanel");
			gp.GraphPanel.create(context.extensionPath);
			gp.GraphPanel.currentPanel!.initialiseGraph(data, wheelSensitivity(), {
				source: "json",
			});
		}),
	);
	// Revive the graph panel across window reloads. The webview persists the
	// request parameters via setState; the data itself isn't persisted, so
	// server graphs are re-requested and JSON imports are re-prompted.
	void import("./graphPanel")
		.then((gp) => {
			context.subscriptions.push(
				window.registerWebviewPanelSerializer(gp.GraphPanel.viewType, {
					async deserializeWebviewPanel(webviewPanel, state) {
						const persisted = state as GraphPanelState | undefined;
						try {
							const panel = gp.GraphPanel.restore(
								context.extensionPath,
								webviewPanel,
							);
							if (persisted?.source === "server" && persisted.entityType) {
								if (!serverProvidesGraphData(client)) {
									window.showWarningMessage(
										"CWTools: this language server doesn't provide graph data, so the graph can't be restored.",
									);
									return;
								}
								const depth = persisted.depth ?? currentGraphDepth;
								const data = await getGraphData(persisted.entityType, depth);
								panel.initialiseGraph(data, wheelSensitivity(), {
									source: "server",
									entityType: persisted.entityType,
									depth,
								});
							} else if (persisted?.source === "json") {
								const uri = await window.showOpenDialog({
									filters: { Json: ["json"] },
								});
								if (!uri) {
									window.showInformationMessage(
										"CWTools: graph data from a JSON export isn't persisted across reloads. " +
											"Run 'CWTools: Recreate graph from json' to rebuild it.",
									);
									return;
								}
								const bytes = await vscode.workspace.fs.readFile(uri[0]);
								const data = new TextDecoder("utf-8").decode(bytes);
								panel.initialiseGraph(data, wheelSensitivity(), {
									source: "json",
								});
							} else if (serverProvidesGraphData(client)) {
								// No persisted state (e.g. a reload before the first render):
								// fall back to the last active entity type.
								const entityType = tracker.getLatestType();
								const data = await getGraphData(entityType, currentGraphDepth);
								panel.initialiseGraph(data, wheelSensitivity(), {
									source: "server",
									entityType,
									depth: currentGraphDepth,
								});
							} else {
								window.showInformationMessage(
									"CWTools: graph data isn't persisted across reloads. " +
										"Run 'CWTools: Show graph' to rebuild the graph.",
								);
							}
						} catch (err) {
							logError("graph panel restore failed", err);
						}
					},
				}),
			);
		})
		.catch((err: unknown) =>
			logError("graph panel serializer registration failed", err),
		);
	// cacheVanilla / clearAllCaches / reindexWorkspace are NOT registered here:
	// the language client registers them from the server's
	// executeCommandProvider, and the executeCommand middleware surfaces
	// their results.

	// Fetch the server's accumulated profiling report and save it to a file.
	// The server only fills the buffer when launched with CWTOOLS_PROFILE=1
	// (the cwtools.profiling setting), so prompt to enable it if empty.
	context.subscriptions.push(
		commands.registerCommand("cwtools.exportProfilingLog", async () => {
			let log: unknown;
			try {
				log = await client.sendRequest(ExecuteCommandRequest.type, {
					command: "exportProfilingLog",
					arguments: [],
				});
			} catch (err) {
				const msg = errorMessage(err);
				window.showErrorMessage(
					`CWTools: could not fetch profiling log: ${msg}`,
				);
				return;
			}
			if (typeof log !== "string" || log.length === 0) {
				window.showWarningMessage(
					"CWTools: profiling log is empty. Turn on 'cwtools.profiling', reload the window, reproduce the slowdown, then export.",
				);
				return;
			}
			const uri = await window.showSaveDialog({
				filters: { Log: ["log", "txt"] },
				saveLabel: "Export CWTools profiling log",
			});
			if (!uri) {
				return;
			}
			await workspace.fs.writeFile(uri, Buffer.from(log, "utf8"));
			window.showInformationMessage(
				`CWTools: profiling log written to ${uri.fsPath}`,
			);
		}),
	);

	context.subscriptions.push(
		commands.registerCommand("cwtools.fixAllWorkspace", async () => {
			if (!serverProvidesFixAll(client)) {
				window.showWarningMessage(
					"CWTools: this language server doesn't support fixing the workspace. " +
						"Update the language server to enable it.",
				);
				return;
			}
			try {
				const result: unknown = await client.sendRequest(
					ExecuteCommandRequest.type,
					{
						command: "fixAllWorkspace",
						arguments: [],
					},
				);
				if (typeof result === "string" && result.length > 0) {
					window.showInformationMessage(`CWTools: ${result}`);
				}
			} catch (err) {
				const msg = errorMessage(err);
				window.showErrorMessage(`CWTools: fixAllWorkspace failed: ${msg}`);
			}
		}),
	);
}
