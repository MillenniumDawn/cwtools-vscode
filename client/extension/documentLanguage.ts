import * as vscode from "vscode";
import type { ExtensionContext } from "vscode";
import { workspace, window, commands } from "vscode";
import { ExecuteCommandRequest } from "vscode-languageclient/node";
import type { LanguageClient } from "vscode-languageclient/node";
import { shouldNotifyFocus, pendingProcessDelayMs } from "./focusTracking";
import { logError, logInfo } from "./logger";

export interface EditorTracker {
	getLatestType(): string;
	// Classify the editor that was already focused when the extension activated.
	// onDidChangeActiveTextEditor never fires for it, so without this the file
	// type stays unknown until the user switches tabs once. Call it after the
	// client is running, since it makes a getFileTypes request.
	classifyActiveEditor(): Promise<void>;
}

// Trailing debounce on tab switches: rapid cycling otherwise sends a
// didFocusFile notification + getFileTypes request per switch.
const ACTIVE_EDITOR_DEBOUNCE_MS = 200;

export async function registerDocumentLanguage(
	context: ExtensionContext,
	client: LanguageClient,
	languageId: string,
): Promise<EditorTracker> {
	const didFocusFile = "didFocusFile";
	let latestType: string = "";
	let getFileTypesInFlight = false;
	let pendingEditor: vscode.TextEditor | undefined;
	let lastFocusUri: string | undefined;
	const getFileTypesTimeoutMs = 5000;
	const getFileTypesBackoffMs = 2000;

	// The static filenamePatterns in package.json only match game files under a
	// folder named like the game ("hearts of iron iv"), so a mod workspace with
	// any other name opens its .txt files as plaintext (no grammar, no LSP).
	// Upgrade plaintext docs that look like game script to the detected language.
	// Scoped to the usual game dirs (and known extensions) so unrelated .txt
	// notes and scratch buffers aren't hijacked, in both the concrete-game and
	// generic "paradox" cases.
	const gameScriptDirs =
		/[\\/](events|common|map|map_data|gfx|interface|history|localisation|localisation_synced|localization|music|sound|portraits|prescripted_countries|tutorial|decisions|missions)[\\/]/i;
	function looksLikeGameScript(doc: vscode.TextDocument): boolean {
		if (doc.uri.scheme !== "file") return false;
		const p = doc.uri.fsPath;
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

	async function didChangeActiveTextEditor(
		editor: vscode.TextEditor | undefined,
	): Promise<void> {
		try {
			if (!editor) return;
			const editorPath = editor.document.uri.toString();
			await upgradePlaintextDocument(editor.document);
			if (
				editor.document.languageId === languageId &&
				shouldNotifyFocus(editorPath, lastFocusUri)
			) {
				await client.sendNotification(didFocusFile, { uri: editorPath });
				lastFocusUri = editorPath;
			}
			// Guard against rapid tab switches piling up requests to a busy server.
			// Only one getFileTypes request runs at a time; a switch that arrives
			// mid-flight is remembered and processed once the in-flight one settles,
			// so latestType and the cwtoolsGraphFile context can't stay stale on the
			// editor the user actually landed on.
			if (getFileTypesInFlight) {
				pendingEditor = editor;
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
				const data = await client.sendRequest(
					ExecuteCommandRequest.type,
					{ command: "getFileTypes", arguments: [editorPath] },
					cts.token,
				);
				if (data && data[0]) {
					latestType = data[0];
					await commands.executeCommand("setContext", "cwtoolsGraphFile", true);
				} else {
					await commands.executeCommand(
						"setContext",
						"cwtoolsGraphFile",
						false,
					);
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
				await commands.executeCommand("setContext", "cwtoolsGraphFile", false);
			} finally {
				clearTimeout(timeoutTimer);
				cts.dispose();
				// After a timeout, cool down before draining pendingEditor so a
				// stalled server isn't re-hit once per timeout window. The in-flight
				// guard stays held through the wait, so switches during the cooldown
				// coalesce into pendingEditor (freshest wins). A settled response
				// drains immediately.
				const delay = pendingProcessDelayMs(timedOut, getFileTypesBackoffMs);
				if (delay > 0) {
					await new Promise<void>((resolve) => setTimeout(resolve, delay));
				}
				getFileTypesInFlight = false;
			}
			if (pendingEditor) {
				const next = pendingEditor;
				pendingEditor = undefined;
				await didChangeActiveTextEditor(next);
			}
		} catch (err) {
			logError("didChangeActiveTextEditor failed", err);
		}
	}

	let debounceTimer: NodeJS.Timeout | undefined;
	context.subscriptions.push(
		window.onDidChangeActiveTextEditor((editor) => {
			if (debounceTimer) clearTimeout(debounceTimer);
			debounceTimer = setTimeout(
				() => void didChangeActiveTextEditor(editor),
				ACTIVE_EDITOR_DEBOUNCE_MS,
			);
		}),
	);

	await Promise.all(workspace.textDocuments.map(upgradePlaintextDocument));
	context.subscriptions.push(
		workspace.onDidOpenTextDocument(upgradePlaintextDocument),
	);

	return {
		getLatestType: () => latestType,
		classifyActiveEditor: () =>
			didChangeActiveTextEditor(window.activeTextEditor),
	};
}
