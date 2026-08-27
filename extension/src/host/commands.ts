import * as vscode from "vscode";
import type { ExtensionContext } from "vscode";
import { workspace, window, commands, l10n } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";
import {
	getGraphData,
	type GraphData,
	type GraphPanelState,
} from "../common/graphTypes";
import {
	graphDataAvailable,
	fixAllWorkspaceAvailable,
	formatWorkspaceAvailable,
} from "./graphAvailability";
import type { EditorTracker } from "./documentLanguage";
import { errorMessage, logError, outputChannel } from "./logger";
import { runCancellableExecuteCommand } from "./commandProgress";
import { showServerBlockedDialog } from "./serverBlockedDialog";
// Type-only: the graph panel stays lazily imported (it pulls in the webview
// plumbing), and `import type` is erased, so naming its shape here doesn't
// pull it into the activation path.
import type * as graphPanelModule from "./graphPanel";

type GraphPanelModule = typeof graphPanelModule;

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

function serverProvidesFormatWorkspace(client: LanguageClient): boolean {
	return formatWorkspaceAvailable(
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
	void commands.executeCommand(
		"setContext",
		"cwtoolsFormatWorkspaceAvailable",
		serverProvidesFormatWorkspace(client),
	);
}

function protocolRecord(value: unknown): Record<string, unknown> | undefined {
	return value !== null && typeof value === "object"
		? (value as Record<string, unknown>)
		: undefined;
}

function protocolPosition(value: unknown): vscode.Position | undefined {
	const position = protocolRecord(value);
	const line = position?.line;
	const character = position?.character;
	if (
		typeof line !== "number" ||
		typeof character !== "number" ||
		!Number.isSafeInteger(line) ||
		!Number.isSafeInteger(character) ||
		line < 0 ||
		character < 0
	) {
		return undefined;
	}
	return new vscode.Position(line, character);
}

function protocolLocation(value: unknown): vscode.Location | undefined {
	const location = protocolRecord(value);
	const uri = location?.uri;
	const range = protocolRecord(location?.range);
	if (typeof uri !== "string" || range === undefined) {
		return undefined;
	}
	const start = protocolPosition(range.start);
	const end = protocolPosition(range.end);
	if (start === undefined || end === undefined) {
		return undefined;
	}
	return new vscode.Location(
		vscode.Uri.parse(uri),
		new vscode.Range(start, end),
	);
}

async function showReferences(
	uriValue: unknown,
	positionValue: unknown,
	locationsValue: unknown,
): Promise<void> {
	if (typeof uriValue !== "string" || !Array.isArray(locationsValue)) {
		return;
	}
	const position = protocolPosition(positionValue);
	const locations = locationsValue.map(protocolLocation);
	if (
		position === undefined ||
		locations.some((location) => location === undefined)
	) {
		return;
	}
	await commands.executeCommand(
		"editor.action.showReferences",
		vscode.Uri.parse(uriValue),
		position,
		locations,
	);
}

export function registerCommands(
	context: ExtensionContext,
	client: LanguageClient,
	tracker: EditorTracker,
	serverExe: string | undefined,
): void {
	context.subscriptions.push(
		commands.registerCommand("cwtools.showReferences", showReferences),
	);
	context.subscriptions.push(
		commands.registerCommand("cwtools.showOutput", () => {
			outputChannel.show();
		}),
	);
	let restartInFlight = false;
	context.subscriptions.push(
		commands.registerCommand("cwtools.restartServer", async () => {
			if (restartInFlight) {
				return;
			}
			restartInFlight = true;
			try {
				// restart()'s stop() half throws unless the client is Running.
				// After the error handler gives up (Stopped/StartFailed) the
				// library clears its start promise, so start() is the recovery
				// path; while Starting it just joins the in-flight start.
				if (client.isRunning()) {
					await client.restart();
				} else {
					await client.start();
				}
				publishCommandAvailability(client);
			} catch (err) {
				if (showServerBlockedDialog(err, serverExe)) {
					return;
				}
				const msg = errorMessage(err);
				window.showErrorMessage(
					l10n.t(
						"CWTools: failed to restart the language server: {0}",
						msg,
					),
				);
			} finally {
				restartInFlight = false;
			}
		}),
	);
	let currentGraphDepth = 3;
	const wheelSensitivity = (): number =>
		workspace.getConfiguration("cwtools.graph").get("zoomSensitivity") ?? 1;
	const showGraph = async function () {
		if (!serverProvidesGraphData(client)) {
			window.showWarningMessage(
				l10n.t(
					"CWTools: this language server doesn't provide graph data, so a graph can only be opened from a saved export. Run 'cwtools: Recreate graph from json'.",
				),
			);
			return;
		}
		const entityType = tracker.getLatestType();
		let loaded: [GraphPanelModule, GraphData];
		try {
			loaded = await Promise.all([
				import("./graphPanel"),
				getGraphData(entityType, currentGraphDepth),
			]);
		} catch (err) {
			// The graph build now runs under a cancellable notification, so
			// Cancel lands here. Opening an empty panel would be worse than
			// doing nothing.
			if (err instanceof vscode.CancellationError) {
				return;
			}
			throw err;
		}
		const [gp, graphData] = loaded;
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
					l10n.t(
						"CWTools: this language server doesn't provide graph data, so the graph depth can't be changed.",
					),
				);
				return;
			}
			const res = await window.showInputBox({
				placeHolder: l10n.t("default: 3"),
				prompt: l10n.t(
					"Set graph depth (how many connections to go back from this file)",
				),
				value: currentGraphDepth.toString(),
				validateInput: (v: string) =>
					Number.isInteger(Number(v))
						? undefined
						: l10n.t("Please enter a number"),
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
										l10n.t(
											"CWTools: this language server doesn't provide graph data, so the graph can't be restored.",
										),
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
										l10n.t(
											"CWTools: graph data from a JSON export isn't persisted across reloads. Run 'CWTools: Recreate graph from json' to rebuild it.",
										),
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
									l10n.t(
										"CWTools: graph data isn't persisted across reloads. Run 'CWTools: Show graph' to rebuild the graph.",
									),
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
				log = await runCancellableExecuteCommand(
					client,
					"exportProfilingLog",
					[],
					l10n.t("CWTools: Export profiling log"),
				);
			} catch (err) {
				if (err instanceof vscode.CancellationError) {
					return;
				}
				const msg = errorMessage(err);
				window.showErrorMessage(
					l10n.t("CWTools: could not fetch profiling log: {0}", msg),
				);
				return;
			}
			if (typeof log !== "string" || log.length === 0) {
				window.showWarningMessage(
					l10n.t(
						"CWTools: profiling log is empty. Turn on 'cwtools.profiling', reload the window, reproduce the slowdown, then export.",
					),
				);
				return;
			}
			const uri = await window.showSaveDialog({
				filters: { Log: ["log", "txt"] },
				saveLabel: l10n.t("Export CWTools profiling log"),
			});
			if (!uri) {
				return;
			}
			await workspace.fs.writeFile(uri, Buffer.from(log, "utf8"));
			window.showInformationMessage(
				l10n.t("CWTools: profiling log written to {0}", uri.fsPath),
			);
		}),
	);

	context.subscriptions.push(
		commands.registerCommand("cwtools.fixAllWorkspace", async () => {
			if (!serverProvidesFixAll(client)) {
				window.showWarningMessage(
					l10n.t(
						"CWTools: this language server doesn't support fixing the workspace. Update the language server to enable it.",
					),
				);
				return;
			}
			try {
				const result = await runCancellableExecuteCommand(
					client,
					"fixAllWorkspace",
					[],
					l10n.t("CWTools: Fix all auto-fixable problems in workspace"),
					// Lands as a single workspace edit from an already-computed
					// snapshot: there is no half-applied state to stop in, so
					// Cancel stays the `$/cancelRequest` fallback.
					{ serverProgress: false },
				);
				if (typeof result === "string" && result.length > 0) {
					window.showInformationMessage(`CWTools: ${result}`);
				}
			} catch (err) {
				if (err instanceof vscode.CancellationError) {
					return;
				}
				const msg = errorMessage(err);
				window.showErrorMessage(
					l10n.t("CWTools: fixAllWorkspace failed: {0}", msg),
				);
			}
		}),
	);

	context.subscriptions.push(
		commands.registerCommand("cwtools.formatWorkspace", async () => {
			if (!serverProvidesFormatWorkspace(client)) {
				window.showWarningMessage(
					l10n.t(
						"CWTools: this language server doesn't support formatting the workspace. Update the language server to enable it.",
					),
				);
				return;
			}
			try {
				const result = await runCancellableExecuteCommand(
					client,
					"formatWorkspace",
					[],
					l10n.t("CWTools: Format workspace"),
				);
				if (typeof result === "string" && result.length > 0) {
					window.showInformationMessage(`CWTools: ${result}`);
				}
			} catch (err) {
				if (err instanceof vscode.CancellationError) {
					return;
				}
				const msg = errorMessage(err);
				window.showErrorMessage(
					l10n.t("CWTools: formatWorkspace failed: {0}", msg),
				);
			}
		}),
	);
}
