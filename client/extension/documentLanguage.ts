import * as vscode from 'vscode';
import type { ExtensionContext } from 'vscode';
import { workspace, window, commands } from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import { NotificationType, ExecuteCommandRequest } from 'vscode-languageclient/node';
import { logError } from './logger';

interface DidFocusFile { uri: string }

export interface EditorTracker {
	getLatestType(): string;
}

// Trailing debounce on tab switches: rapid cycling otherwise sends a
// didFocusFile notification + getFileTypes request per switch.
const ACTIVE_EDITOR_DEBOUNCE_MS = 200;

export async function registerDocumentLanguage(
	context: ExtensionContext,
	client: LanguageClient,
	languageId: string
): Promise<EditorTracker> {
	const didFocusFile = new NotificationType<DidFocusFile>('didFocusFile');
	let latestType : string = '';
	let getFileTypesInFlight = false;
	let pendingEditor : vscode.TextEditor | undefined;
	const getFileTypesTimeoutMs = 5000;

	// The static filenamePatterns in package.json only match game files under a
	// folder named like the game ("hearts of iron iv"), so a mod workspace with
	// any other name opens its .txt files as plaintext (no grammar, no LSP).
	// Upgrade plaintext docs that look like game script to the detected language.
	// Scoped to the usual game dirs (and known extensions) so unrelated .txt
	// notes and scratch buffers aren't hijacked, in both the concrete-game and
	// generic "paradox" cases.
	const gameScriptDirs = /[\\/](events|common|map|map_data|gfx|interface|history|localisation|localisation_synced|localization|music|sound|portraits|prescripted_countries|tutorial|decisions|missions)[\\/]/i;
	function looksLikeGameScript(doc : vscode.TextDocument): boolean {
		if (doc.uri.scheme !== 'file') return false;
		const p = doc.uri.fsPath;
		if (/\.(gui|gfx|asset|sfx)$/i.test(p)) return true;
		return /\.txt$/i.test(p) && gameScriptDirs.test(p);
	}
	async function upgradePlaintextDocument(doc : vscode.TextDocument): Promise<void> {
		if (doc.languageId !== "plaintext") return;
		if (!looksLikeGameScript(doc)) return;
		await vscode.languages.setTextDocumentLanguage(doc, languageId)
	}

	async function didChangeActiveTextEditor(editor : vscode.TextEditor | undefined): Promise<void> {
		if (!editor) return;
		const editorPath = editor.document.uri.toString();
		await upgradePlaintextDocument(editor.document);
		if (editor.document.languageId === languageId) {
			await client.sendNotification(didFocusFile, {uri: editorPath});
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
		try {
			const data = await Promise.race([
				client.sendRequest(
					ExecuteCommandRequest.type,
					{ command: "getFileTypes", arguments: [editorPath] }
				),
				new Promise<never>((_, reject) =>
					setTimeout(() => reject(new Error('getFileTypes request timed out')), getFileTypesTimeoutMs)
				)
			]);
			if (data && data[0]) {
				latestType = data[0];
				await commands.executeCommand('setContext', 'cwtoolsGraphFile', true);
			} else {
				await commands.executeCommand('setContext', 'cwtoolsGraphFile', false);
			}
		} catch (err) {
			logError('didChangeActiveTextEditor getFileTypes failed', err);
			await commands.executeCommand('setContext', 'cwtoolsGraphFile', false);
		} finally {
			getFileTypesInFlight = false;
		}
		if (pendingEditor) {
			const next = pendingEditor;
			pendingEditor = undefined;
			await didChangeActiveTextEditor(next);
		}
	}

	let debounceTimer: NodeJS.Timeout | undefined;
	context.subscriptions.push(window.onDidChangeActiveTextEditor(editor => {
		if (debounceTimer) clearTimeout(debounceTimer);
		debounceTimer = setTimeout(() => void didChangeActiveTextEditor(editor), ACTIVE_EDITOR_DEBOUNCE_MS);
	}));

	await Promise.all(workspace.textDocuments.map(upgradePlaintextDocument));
	context.subscriptions.push(workspace.onDidOpenTextDocument(upgradePlaintextDocument));

	return { getLatestType: () => latestType };
}
