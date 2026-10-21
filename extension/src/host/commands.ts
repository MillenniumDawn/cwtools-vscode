import * as vscode from "vscode";
import * as path from "path";
import type { Disposable, ExtensionContext } from "vscode";
import { workspace, window, commands, l10n } from "vscode";
import { State, type LanguageClient } from "vscode-languageclient/node";
import {
	getGraphData,
	type GraphData,
	type GraphPanelState,
} from "../common/graphTypes";
import { serverCommand } from "../common/serverCommandContract";
import {
	graphDataAvailable,
	fixAllWorkspaceAvailable,
	formatWorkspaceAvailable,
	validateWorkspaceAvailable,
} from "./graphAvailability";
import type { EditorTracker } from "./documentLanguage";
import { errorMessage, logError, outputChannel } from "./logger";
import { runCancellableExecuteCommand } from "./commandProgress";
import { showServerBlockedDialog } from "./serverBlockedDialog";
import { GAMES } from "./games";
import { discoverVanillaInstallations } from "./baseGame";
import { PENDING_VANILLA_CACHE_KEY } from "./reindexSettings";
// Type-only: the graph panel stays lazily imported (it pulls in the webview
// plumbing), and `import type` is erased, so naming its shape here doesn't
// pull it into the activation path.
import type * as graphPanelModule from "./graphPanel";

type GraphPanelModule = typeof graphPanelModule;

interface PendingVanillaCache {
	gameId: string;
	folder: string;
	offerCache: boolean;
}

function offerPendingVanillaCacheAfterReady(
	context: ExtensionContext,
	client: LanguageClient,
	activeGame: string,
): void {
	const pending = context.workspaceState.get<PendingVanillaCache>(
		PENDING_VANILLA_CACHE_KEY,
	);
	if (!pending) return;
	if (!pending.offerCache || pending.gameId !== activeGame) {
		void context.workspaceState.update(PENDING_VANILLA_CACHE_KEY, undefined);
		return;
	}
	const configured = workspace
		.getConfiguration("cwtools")
		.get<string>(`cache.${pending.gameId}`);
	if (configured !== pending.folder) {
		void context.workspaceState.update(PENDING_VANILLA_CACHE_KEY, undefined);
		return;
	}
	const offer = async (): Promise<void> => {
		if (!client.isRunning()) return;
		// Clear before presenting the prompt so reactivation cannot offer it twice.
		await context.workspaceState.update(PENDING_VANILLA_CACHE_KEY, undefined);
		const action = await window.showInformationMessage(
			l10n.t(
				"The {0} base game folder is active. Regenerate its vanilla cache now?",
				GAMES.find((game) => game.id === pending.gameId)?.display ??
					pending.gameId,
			),
			l10n.t("Regenerate vanilla cache"),
		);
		if (action === l10n.t("Regenerate vanilla cache")) {
			await commands.executeCommand(serverCommand("cacheVanilla"));
		}
	};
	const reportFailure = (err: unknown): void =>
		logError("Failed to offer vanilla cache regeneration", err);
	if (client.isRunning()) {
		void offer().catch(reportFailure);
		return;
	}
	// The language client exposes this as a VS Code event; its declaration is
	// typed as `any` in the installed 10.x package under skipLibCheck.
	// eslint-disable-next-line @typescript-eslint/no-unsafe-call
	const stateChange = client.onDidChangeState((event: { newState: State }) => {
		if (event.newState !== State.Running) return;
		stateChange.dispose();
		void offer().catch(reportFailure);
	}) as Disposable;
	context.subscriptions.push(stateChange);
}

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

function serverProvidesValidateWorkspace(client: LanguageClient): boolean {
	return validateWorkspaceAvailable(
		client.initializeResult?.capabilities.executeCommandProvider?.commands,
	);
}

// Gates the palette entries and the editor-title button. The client state
// handler calls it whenever the client reaches Running, when the server's
// capabilities are known.
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
	void commands.executeCommand(
		"setContext",
		"cwtoolsValidateWorkspaceAvailable",
		serverProvidesValidateWorkspace(client),
	);
}

export function clearCommandAvailability(): void {
	void commands.executeCommand("setContext", "cwtoolsGraphAvailable", false);
	void commands.executeCommand("setContext", "cwtoolsFixAllAvailable", false);
	void commands.executeCommand(
		"setContext",
		"cwtoolsFormatWorkspaceAvailable",
		false,
	);
	void commands.executeCommand(
		"setContext",
		"cwtoolsValidateWorkspaceAvailable",
		false,
	);
}

function requireRunningServer(client: LanguageClient): boolean {
	if (client.isRunning()) {
		return true;
	}
	window.showWarningMessage(
		l10n.t(
			"CWTools: the language server is stopped. Run 'CWTools: Restart Server' to start it again.",
		),
	);
	return false;
}

// The server rejects any depth below 1, so the prompt and the stored depth share this.
function parseGraphDepth(value: string | undefined): number | undefined {
	const text = value?.trim();
	if (!text || !/^\d+$/.test(text)) {
		return undefined;
	}
	const depth = Number(text);
	return Number.isSafeInteger(depth) && depth >= 1 ? depth : undefined;
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
	activeGame: string,
): void {
	context.subscriptions.push(
		commands.registerCommand("cwtools.showReferences", showReferences),
	);
	context.subscriptions.push(
		commands.registerCommand("cwtools.showOutput", () => {
			outputChannel.show();
		}),
	);
	context.subscriptions.push(
		commands.registerCommand("cwtools.selectBaseGame", async () => {
			const game = await window.showQuickPick(
				GAMES.map((item) => ({ label: item.display, id: item.id })),
				{ placeHolder: l10n.t("Select the base game to configure") },
			);
			if (!game) return;

			const definition = GAMES.find((item) => item.id === game.id);
			if (!definition) return;
			const installations = discoverVanillaInstallations(definition);
			const browse: { label: string; browse?: true; folder?: string } = {
				label: l10n.t("Browse for a game folder…"),
				browse: true,
			};
			const choices: Array<{ label: string; browse?: true; folder?: string }> = [
				...installations.map((folder) => ({ label: folder, folder })),
				browse,
			];
			const selection = await window.showQuickPick(
				choices,
				{
					placeHolder: installations.length
						? l10n.t("Select a {0} installation", definition.display)
						: l10n.t("No Steam installation found. Browse for the {0} game folder", definition.display),
				},
			);
			if (!selection) return;

			let folder: string | undefined;
			if (selection.browse === true) {
				const picked = await window.showOpenDialog({
					canSelectFiles: false,
					canSelectFolders: true,
					canSelectMany: false,
					openLabel: l10n.t("Select {0} game folder", definition.display),
				});
				folder = picked?.[0]?.fsPath;
			} else {
				folder = selection.folder;
			}
			if (!folder) return;

			await context.workspaceState.update(PENDING_VANILLA_CACHE_KEY, {
				gameId: definition.id,
				folder,
				offerCache: definition.id === activeGame,
			} satisfies PendingVanillaCache);
			try {
				await workspace
					.getConfiguration("cwtools")
					.update(`cache.${definition.id}`, folder, vscode.ConfigurationTarget.Global);
			} catch (err) {
				await context.workspaceState.update(PENDING_VANILLA_CACHE_KEY, undefined);
				throw err;
			}
			const reloadWindow = l10n.t("Reload Window");
			const reload = await window.showInformationMessage(
				definition.id === activeGame
					? l10n.t(
							"Saved the {0} base game folder. Reload the window to apply it; after the server starts, CWTools will offer to regenerate its vanilla cache.",
							definition.display,
						)
					: l10n.t(
							"Saved the {0} base game folder. Reload the window to apply it. The current server is for {1}; use the picker in a {0} workspace to regenerate this game's vanilla cache.",
							definition.display,
							GAMES.find((game) => game.id === activeGame)?.display ??
								activeGame,
						),
				reloadWindow,
			);
			if (reload === reloadWindow) {
				await commands.executeCommand("workbench.action.reloadWindow");
			}
		}),
	);
	offerPendingVanillaCacheAfterReady(context, client, activeGame);
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
			} catch (err) {
				if (showServerBlockedDialog(err, serverExe)) {
					return;
				}
				const msg = errorMessage(err);
				window.showErrorMessage(
					l10n.t("CWTools: failed to restart the language server: {0}", msg),
				);
			} finally {
				restartInFlight = false;
			}
		}),
	);
	let currentGraphDepth = 3;
	// Show graph, JSON imports and reload restoration share this generation.
	// Imports commit on file selection, before reading; cancelled dialogs leave
	// pending work eligible to finish. Only the latest committed request may
	// render or persist its source. A failed request never revives older results.
	let latestGraphRequest = 0;
	const wheelSensitivity = (): number =>
		workspace.getConfiguration("cwtools.graph").get("zoomSensitivity") ?? 1;
	const reportNoGraph = (): void => {
		window.showInformationMessage(l10n.t("CWTools: no graph for this file."));
	};
	const showGraph = async function () {
		if (!requireRunningServer(client)) {
			return;
		}
		if (!serverProvidesGraphData(client)) {
			window.showWarningMessage(
				l10n.t(
					"CWTools: this language server doesn't provide graph data, so a graph can only be opened from a saved export. Run 'cwtools: Recreate graph from json'.",
				),
			);
			return;
		}
		const entityType = tracker.getLatestType();
		if (!entityType) {
			reportNoGraph();
			return;
		}
		const depth = currentGraphDepth;
		const requestId = ++latestGraphRequest;
		let loaded: [GraphPanelModule, GraphData];
		try {
			loaded = await Promise.all([
				import("./graphPanel"),
				getGraphData(entityType, depth),
			]);
		} catch (err) {
			if (requestId !== latestGraphRequest) {
				return;
			}
			// The graph build now runs under a cancellable notification, so
			// Cancel lands here. Opening an empty panel would be worse than
			// doing nothing.
			if (!client.isRunning()) {
				requireRunningServer(client);
				return;
			}
			if (err instanceof vscode.CancellationError) {
				return;
			}
			throw err;
		}
		if (requestId !== latestGraphRequest) {
			return;
		}
		const [gp, graphData] = loaded;
		gp.GraphPanel.create(context.extensionPath);
		gp.GraphPanel.currentPanel!.initialiseGraph(graphData, wheelSensitivity(), {
			source: "server",
			entityType,
			depth,
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
			if (!requireRunningServer(client)) {
				return;
			}
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
					parseGraphDepth(v) === undefined
						? l10n.t("Please enter a whole number of at least 1")
						: undefined,
			});
			const depth = parseGraphDepth(res);
			if (depth !== undefined) {
				currentGraphDepth = depth;
				await showGraph();
			}
		}),
	);
	context.subscriptions.push(
		commands.registerCommand("cwtools.graphFromJson", async () => {
			const uri = await window.showOpenDialog({ filters: { Json: ["json"] } });
			if (!uri?.length) {
				return;
			}
			// Selecting a file commits an import. Cancelling the dialog leaves
			// pending work alone; after selection, even a failed read supersedes it.
			const requestId = ++latestGraphRequest;
			const bytes = await vscode.workspace.fs.readFile(uri[0]);
			const data = new TextDecoder("utf-8").decode(bytes);
			const gp = await import("./graphPanel");
			if (requestId !== latestGraphRequest) {
				return;
			}
			gp.GraphPanel.create(context.extensionPath);
			gp.GraphPanel.currentPanel!.initialiseGraph(data, wheelSensitivity(), {
				source: "json",
				fileName: path.basename(uri[0].fsPath),
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
							let requestId = persisted?.source === "json"
								? latestGraphRequest
								: ++latestGraphRequest;
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
								if (requestId !== latestGraphRequest) {
									return;
								}
								panel.initialiseGraph(data, wheelSensitivity(), {
									source: "server",
									entityType: persisted.entityType,
									depth,
								});
							} else if (persisted?.source === "json") {
								const uri = await window.showOpenDialog({
									filters: { Json: ["json"] },
								});
								if (!uri?.length) {
									window.showInformationMessage(
										l10n.t(
											"CWTools: graph data from a JSON export isn't persisted across reloads. Run 'CWTools: Recreate graph from json' to rebuild it.",
										),
									);
									return;
								}
								// Restoration uses the same file-selection commitment point.
								requestId = ++latestGraphRequest;
								const bytes = await vscode.workspace.fs.readFile(uri[0]);
								const data = new TextDecoder("utf-8").decode(bytes);
								if (requestId !== latestGraphRequest) {
									return;
								}
								// Render in place: create() would reveal the revived panel in
								// the active editor's column. Use it only once the panel is gone.
								if (gp.GraphPanel.currentPanel !== panel) {
									gp.GraphPanel.create(context.extensionPath);
								}
								gp.GraphPanel.currentPanel!.initialiseGraph(data, wheelSensitivity(), {
									source: "json",
									fileName: path.basename(uri[0].fsPath),
								});
							} else if (serverProvidesGraphData(client)) {
								// No persisted state (e.g. a reload before the first render):
								// fall back to the last active entity type.
								const entityType = tracker.getLatestType();
								if (!entityType) {
									reportNoGraph();
									return;
								}
								const data = await getGraphData(entityType, currentGraphDepth);
								if (requestId !== latestGraphRequest) {
									return;
								}
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
					serverCommand("exportProfilingLog"),
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
			if (!requireRunningServer(client)) {
				return;
			}
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
					serverCommand("fixAllWorkspace"),
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
				if (!client.isRunning()) {
					requireRunningServer(client);
					return;
				}
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
			if (!requireRunningServer(client)) {
				return;
			}
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
					serverCommand("formatWorkspace"),
					[],
					l10n.t("CWTools: Format workspace"),
				);
				if (typeof result === "string" && result.length > 0) {
					window.showInformationMessage(`CWTools: ${result}`);
				}
			} catch (err) {
				if (!client.isRunning()) {
					requireRunningServer(client);
					return;
				}
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
