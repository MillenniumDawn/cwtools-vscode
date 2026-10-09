import * as vscode from "vscode";
import * as path from "path";
import type { ExtensionContext } from "vscode";
import { workspace, window, commands } from "vscode";
import { ExecuteCommandRequest } from "vscode-languageclient/node";
import type { LanguageClient } from "vscode-languageclient/node";
import { serverCommand } from "../common/serverCommandContract";
import { shouldNotifyFocus, pendingProcessDelayMs } from "./focusTracking";
import { logError, logInfo } from "./logger";
import { isTrustedPath } from "./trustedPaths";

export interface EditorTracker {
	getLatestType(): string;
	// Classify the editor that was already focused when the extension activated.
	// onDidChangeActiveTextEditor never fires for it, so without this the file
	// type stays unknown until the user switches tabs once. Call it after the
	// client is running, since it makes a getFileTypes request.
	classifyActiveEditor(): Promise<void>;
	/** Recheck open documents when the server resolves its base-game install. */
	updateVanillaRoots(this: void, roots: readonly string[]): Promise<void>;
}

// Trailing debounce on tab switches: rapid cycling otherwise sends a
// didFocusFile notification + getFileTypes request per switch.
const ACTIVE_EDITOR_DEBOUNCE_MS = 200;

export async function registerDocumentLanguage(
	context: ExtensionContext,
	client: LanguageClient,
	languageId: string,
	roots: readonly string[] | (() => readonly string[]),
): Promise<EditorTracker> {
	const didFocusFile = "didFocusFile";
	let latestType: string = "";
	let getFileTypesInFlight = false;
	let pendingSwitch: { editor: vscode.TextEditor; gen: number } | undefined;
	let lastFocusUri: string | undefined;
	// Bumped on every active-editor change. A reply or queued switch from an
	// older generation belongs to an editor that is no longer focused.
	let generation = 0;
	let graphFileContext: boolean | undefined;
	const getFileTypesTimeoutMs = 5000;
	const getFileTypesBackoffMs = 2000;
	const readContentRoots = () => typeof roots === "function" ? roots() : roots;
	// A roots callback builds a new array per call, so compare by value.
	const sameRoots = (a: readonly string[], b: readonly string[]) =>
		a.length === b.length && a.every((root, i) => root === b[i]);
	let contentRoots = readContentRoots();
	let vanillaRoots: readonly string[] = [];

	// The static filenamePatterns in package.json only match game files under a
	// folder named like the game ("hearts of iron iv"), so a mod workspace with
	// any other name opens its .txt files as plaintext (no grammar, no LSP).
	// Upgrade plaintext docs that look like game script to the detected language.
	// Only the selected mod, configured parents and the resolved vanilla qualify.
	// Test directory hints relative to those roots, so an ancestor named common
	// cannot turn ordinary notes into script.
	const gameScriptDirs =
		/(?:^|[\\/])(events|common|map|map_data|gfx|interface|history|localisation|localisation_synced|localization|music|sound|portraits|prescripted_countries|tutorial|decisions|missions)[\\/]/i;
	function looksLikeGameScript(doc: vscode.TextDocument): boolean {
		if (doc.uri.scheme !== "file") return false;
		const roots = [...contentRoots, ...vanillaRoots];
		const root = roots.find((root) =>
			isTrustedPath(doc.uri.fsPath, [root]),
		);
		if (!root) return false;
		const p = path.relative(root, doc.uri.fsPath);
		if (/\.(gui|gfx|asset|sfx)$/i.test(p)) return true;
		return /\.txt$/i.test(p) && gameScriptDirs.test(p);
	}
	async function upgradePlaintextDocument(
		doc: vscode.TextDocument,
	): Promise<void> {
		if (doc.languageId !== "plaintext") return;
		if (!looksLikeGameScript(doc)) return;
		await vscode.languages.setTextDocumentLanguage(doc, languageId);
	}

	async function setGraphFile(value: boolean): Promise<void> {
		if (graphFileContext === value) return;
		graphFileContext = value;
		await commands.executeCommand("setContext", "cwtoolsGraphFile", value);
	}

	async function didChangeActiveTextEditor(
		editor: vscode.TextEditor | undefined,
		gen: number,
	): Promise<void> {
		if (gen !== generation) return;
		latestType = "";
		try {
			if (!editor) return;
			const editorPath = editor.document.uri.toString();
			await upgradePlaintextDocument(editor.document);
			if (gen !== generation) return;
			if (
				editor.document.languageId === languageId &&
				shouldNotifyFocus(editorPath, lastFocusUri)
			) {
				await client.sendNotification(didFocusFile, { uri: editorPath });
				lastFocusUri = editorPath;
			}
			if (gen !== generation) return;
			// Guard against rapid tab switches piling up requests to a busy server.
			// Only one getFileTypes request runs at a time; a switch that arrives
			// mid-flight is remembered and processed once the in-flight one settles,
			// so latestType and the cwtoolsGraphFile context can't stay stale on the
			// editor the user actually landed on.
			if (getFileTypesInFlight) {
				pendingSwitch = { editor, gen };
				return;
			}
			getFileTypesInFlight = true;
			let timedOut = false;
			// The timeout guard cancels the request instead of just rejecting locally,
			// so a dead getFileTypes leaves the (possibly saturated) server queue via
			// $/cancelRequest rather than piling up behind it.
			const cts = new vscode.CancellationTokenSource();
			const timeoutTimer = setTimeout(
				() => cts.cancel(),
				getFileTypesTimeoutMs,
			);
			try {
				const data = (await client.sendRequest(
					ExecuteCommandRequest.type,
					{ command: serverCommand("getFileTypes"), arguments: [editorPath] },
					cts.token,
				)) as string[] | undefined;
				if (gen === generation) {
					if (data && data[0]) {
						latestType = data[0];
						await setGraphFile(true);
					} else {
						await setGraphFile(false);
					}
				}
			} catch (err) {
				timedOut = cts.token.isCancellationRequested;
				// A timeout isn't an error; demote it so validate storms don't spam logError.
				if (timedOut) {
					logInfo(
						`didChangeActiveTextEditor getFileTypes timed out after ${getFileTypesTimeoutMs}ms`,
					);
				} else {
					logError("didChangeActiveTextEditor getFileTypes failed", err);
				}
				if (gen === generation) await setGraphFile(false);
			} finally {
				clearTimeout(timeoutTimer);
				cts.dispose();
				// After a timeout, cool down before draining pendingSwitch so a
				// stalled server isn't re-hit once per timeout window. The in-flight
				// guard stays held through the wait, so switches during the cooldown
				// coalesce into pendingSwitch (freshest wins, a superseded one is
				// dropped when drained). A settled response drains immediately.
				const delay = pendingProcessDelayMs(timedOut, getFileTypesBackoffMs);
				if (delay > 0) {
					await new Promise<void>((resolve) => setTimeout(resolve, delay));
				}
				getFileTypesInFlight = false;
			}
			if (pendingSwitch) {
				const next = pendingSwitch;
				pendingSwitch = undefined;
				await didChangeActiveTextEditor(next.editor, next.gen);
			}
		} catch (err) {
			logError("didChangeActiveTextEditor failed", err);
		}
	}

	let debounceTimer: NodeJS.Timeout | undefined;
	context.subscriptions.push(
		window.onDidChangeActiveTextEditor((editor) => {
			const gen = ++generation;
			latestType = "";
			void setGraphFile(false);
			if (debounceTimer) clearTimeout(debounceTimer);
			debounceTimer = setTimeout(
				() => void didChangeActiveTextEditor(editor, gen),
				ACTIVE_EDITOR_DEBOUNCE_MS,
			);
		}),
		{
			dispose: () => {
				clearTimeout(debounceTimer);
				generation++;
			},
		},
	);

	await Promise.all(workspace.textDocuments.map(upgradePlaintextDocument));
	context.subscriptions.push(
		workspace.onDidOpenTextDocument(upgradePlaintextDocument),
	);

	return {
		getLatestType: () => latestType,
		classifyActiveEditor: () =>
			didChangeActiveTextEditor(window.activeTextEditor, ++generation),
		updateVanillaRoots: async (roots) => {
			const nextRoots = readContentRoots();
			if (sameRoots(contentRoots, nextRoots) && sameRoots(vanillaRoots, roots)) return;
			contentRoots = nextRoots;
			vanillaRoots = roots;
			await Promise.all(workspace.textDocuments.map(upgradePlaintextDocument));
		},
	};
}
