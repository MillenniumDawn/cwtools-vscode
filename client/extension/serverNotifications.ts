import * as path from 'path';
import { existsSync as fsExistsSync } from 'fs';
import * as vscode from 'vscode';
import type { ExtensionContext, Disposable } from 'vscode';
import { workspace, window, Uri, WorkspaceEdit, TextEdit, Range, commands } from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import { NotificationType } from 'vscode-languageclient/node';
import type { FileListItem } from './fileExplorer';
import { FileExplorer } from './fileExplorer';
import { GAME_DISPLAY, GAME_FOLDER } from './games';

interface LoadingBarParams { enable: boolean; value: string }
interface DebugStatusBarParams { enable: boolean; value: string }
interface CreateVirtualFile { uri: string; fileContent: string }
interface UpdateFileList { fileList: FileListItem[] }

export function registerServerNotifications(context: ExtensionContext, client: LanguageClient): void {
	const log = client.outputChannel;
	const loadingBarNotification = new NotificationType<LoadingBarParams>('loadingBar');
	const debugStatusBarParamsNotification = new NotificationType<DebugStatusBarParams>('debugBar');
	const createVirtualFile = new NotificationType<CreateVirtualFile>('createVirtualFile');
	const promptReload = new NotificationType<string>('promptReload')
	const forceReload = new NotificationType<string>('forceReload')
	const promptVanillaPath = new NotificationType<string>('promptVanillaPath')
	const updateFileList = new NotificationType<UpdateFileList>('updateFileList');
	let status: Disposable | undefined;
	let fileList : FileListItem[];
	let fileExplorer : FileExplorer;

	client.onNotification(loadingBarNotification, (param: LoadingBarParams) => {
		if (param.enable) {
			if (status !== undefined) {
				status.dispose();
			}
			status = window.setStatusBarMessage(param.value);
		}
		else if (status !== undefined) {
			status.dispose();
			status = undefined;
		}
	})
	const debugStatusBar = window.createStatusBarItem(vscode.StatusBarAlignment.Left);
	context.subscriptions.push(debugStatusBar);
	client.onNotification(debugStatusBarParamsNotification, (param: DebugStatusBarParams) => {
		if (param.enable) {
			debugStatusBar.text = param.value;
			debugStatusBar.show();
		}
		else if (!param.enable) {
			debugStatusBar.hide();
		}
	})
	client.onNotification(createVirtualFile, async (param: CreateVirtualFile) => {
		const uri = Uri.parse(param.uri);
		const doc = await workspace.openTextDocument(uri);
		const edit = new WorkspaceEdit();
		const lastLine = doc.lineCount - 1;
		const lastChar = doc.lineAt(lastLine).text.length;
		edit.set(uri, [new TextEdit(new Range(0, 0, lastLine, lastChar), param.fileContent)]);
		await workspace.applyEdit(edit);
		await window.showTextDocument(uri);
	})
	client.onNotification(promptReload, (param: string) => reloadExtension(param, "Reload"));
	client.onNotification(forceReload, (param: string) => reloadExtension(param, undefined, true));
	client.onNotification(promptVanillaPath, async (param: string) => {
		const gameDisplay = GAME_DISPLAY[param] ?? param;
		const result = await window.showInformationMessage("Please select the vanilla installation folder for " + gameDisplay, "Select folder");
		if (!result) return;
		const uri = await window.showOpenDialog({
			canSelectFiles: false,
			canSelectFolders: true,
			canSelectMany: false,
			openLabel: "Select vanilla installation folder for " + gameDisplay
		});
		if (!uri || uri.length === 0) return;

		const directory = uri[0];
		const game = GAME_FOLDER[path.basename(directory.fsPath).toLowerCase()];
		if (!game) {
			await window.showErrorMessage("The selected folder does not appear to be a supported game folder");
			return;
		}
		// CK3/Vic3/Imperator/EU5 keep `common` under a `game/` subdir, so the
		// folder check has to run against the resolved path, not the root.
		const dir = game.subdir ? path.join(directory.fsPath, game.subdir) : directory.fsPath;
		if (!fsExistsSync(path.join(dir, "common"))) {
			await window.showErrorMessage("The selected folder does not appear to be a supported game folder");
			return;
		}
		log.appendLine("path: " + dir);
		log.appendLine("game: " + game.id);
		await workspace.getConfiguration("cwtools").update("cache." + game.id, dir, true);
		await reloadExtension("Reloading to generate vanilla cache", undefined, true);
	})
	client.onNotification(updateFileList, (params: UpdateFileList) => {
		fileList = params.fileList;
		if (fileExplorer) {
			fileExplorer.refresh(fileList);
		}
		else {
			fileExplorer = new FileExplorer(context, fileList);
		}
	})
}

export async function reloadExtension(prompt: string, buttonText?: string, force? : boolean) {
	const restartAction = buttonText || "Restart";
	const actions = [restartAction];
	if (force) {
		const result = await window.showInformationMessage(prompt, ...actions);
		if (result === restartAction) {
			await commands.executeCommand("cwtools.reloadExtension");
		}
	}
	else {
		const chosenAction = prompt && await window.showInformationMessage(prompt, ...actions);
		if (!prompt || chosenAction === restartAction) {
			await commands.executeCommand("cwtools.reloadExtension");
		}
	}
}
