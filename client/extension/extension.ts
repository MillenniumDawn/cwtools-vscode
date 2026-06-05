/* --------------------------------------------------------------------------------------------
 * Copyright (c) Microsoft Corporation. All rights reserved.
 * Licensed under the MIT License. See License.txt in the project root for license information.
 * ------------------------------------------------------------------------------------------ */
'use strict';

import * as path from 'path';
import * as os from 'os';
import { existsSync as fsExistsSync, statSync as fsStatSync, chmodSync as fsChmodSync, mkdirSync as fsMkdirSync } from 'fs';
import * as vs from 'vscode';
import { workspace, ExtensionContext, window, Disposable, Uri, WorkspaceEdit, TextEdit, Range, commands, env } from 'vscode';
import { LanguageClient, LanguageClientOptions, ServerOptions, TransportKind, NotificationType, ExecuteCommandRequest, RevealOutputChannelOn } from 'vscode-languageclient/node';

import { FileExplorer, FileListItem } from './fileExplorer';
import * as gp from './graphPanel';
import * as exe from './executable';
import { getGraphData } from '../common/graphTypes';
import {
	LANGUAGE_REPOS,
	GAME_DISPLAY,
	GAME_FOLDER,
	detectFromFolder,
	serverExeForEngine,
	runGit,
} from './engine';

const stellarisRemote = `https://github.com/cwtools/cwtools-stellaris-config`;
const eu4Remote = `https://github.com/cwtools/cwtools-eu4-config`;
const hoi4Remote = `https://github.com/cwtools/cwtools-hoi4-config`;
const ck2Remote = `https://github.com/cwtools/cwtools-ck2-config`;
const irRemote = `https://github.com/cwtools/cwtools-ir-config`;
const vic2Remote = `https://github.com/cwtools/cwtools-vic2-config`;
const vic3Remote = `https://github.com/cwtools/cwtools-vic3-config`;
const ck3Remote = `https://github.com/cwtools/cwtools-ck3-config`;
const eu5Remote = `https://github.com/kaiser-chris/cwtools-eu5-config`;

export let defaultClient: LanguageClient;
export async function activate(context: ExtensionContext) {
	let fileList : FileListItem[];
	let fileExplorer : FileExplorer;


	class CwtoolsProvider implements vs.TextDocumentContentProvider
	{
		private disposables: Disposable[] = [];

		constructor(){
			this.disposables.push(
				workspace.registerTextDocumentContentProvider("cwtools", this)
			);
		}
		async provideTextDocumentContent() {
			return '';
		}

		dispose(): void {
			this.disposables.forEach(d => d.dispose());
		}
	}

	// VSCodium reports a placeholder machineId, which is how we tell dev
	// installs from a real VS Code and pick the right cache directory.
	const isDevDir = env.machineId === "someValue.machineId"
	const cacheDir = isDevDir ? path.join(context.globalStorageUri.fsPath, '.cwtools') : path.join(context.extensionPath, '.cwtools')
	if (isDevDir) {
		fsMkdirSync(context.globalStorageUri.fsPath, { recursive: true })
	}

	const init = async function(language : string, isVanillaFolder : boolean) {
		const langConfigDisposable = vs.languages.setLanguageConfiguration(language, { wordPattern : /"?([^\s.]+)"?/ });
		context.subscriptions.push(langConfigDisposable);

		// cwtools.engine picks between the Rust server (default) and the
		// original F# server. If the chosen engine isn't deployed, fall
		// back to whichever one is.
		const requestedEngine = workspace.getConfiguration('cwtools').get<string>('engine') ?? 'rust';
		let activeEngine = requestedEngine;
		let serverExe = serverExeForEngine(context, requestedEngine);
		if (!serverExe) {
			const otherEngine = requestedEngine === 'fsharp' ? 'rust' : 'fsharp';
			const fallback = serverExeForEngine(context, otherEngine);
			if (fallback) {
				activeEngine = otherEngine;
				serverExe = fallback;
				window.showWarningMessage(
					`CWTools: '${requestedEngine}' engine binary missing, falling back to '${otherEngine}'. ` +
					`Build the '${requestedEngine}' server or change cwtools.engine.`);
			} else {
				await window.showErrorMessage(
					`CWTools: no language server binary found for engine '${requestedEngine}'. ` +
					`Re-install the extension or build the server.`);
				return;
			}
		}
		console.log(`[CWTools] Using '${activeEngine}' engine: ${serverExe}`);

		if (os.platform() !== 'win32') {
			try {
				const stat = fsStatSync(serverExe);
				if ((stat.mode & 0o111) === 0) {
					fsChmodSync(serverExe, 0o755);
				}
			} catch (e) {
				console.error('[CWTools] stat/chmod error on server binary:', e);
			}
		}

		const repoPath = LANGUAGE_REPOS[language];
		if (!repoPath) {
			console.warn(`[CWTools] No config repository for language "${language}"; rule cloning skipped.`);
		}
		console.log(`${language} ${repoPath || '(no remote)'}`);

		fsMkdirSync(cacheDir, { recursive: true });
		const languageRulesCache = path.join(cacheDir, language);

		const manualRules = workspace.getConfiguration('cwtools').get<string>('rules_folder');
		const effectiveRulesCache = (manualRules && fsExistsSync(manualRules)) ? manualRules : languageRulesCache;
		// F# server appends the per-game subdir itself; Rust server gets the leaf.
		const rulesCacheForServer = activeEngine === 'fsharp' ? cacheDir : effectiveRulesCache;
		if (manualRules && fsExistsSync(manualRules)) {
			console.log(`[CWTools] Using manual rules folder: ${manualRules}`);
		} else if (repoPath) {
			try {
				const gitDir = path.join(languageRulesCache, '.git');
				if (!fsExistsSync(gitDir)) {
					console.log(`[CWTools] Cloning rules from ${repoPath} into ${languageRulesCache}`);
					await runGit(['clone', '--depth', '1', repoPath, languageRulesCache]);
				} else {
					console.log(`[CWTools] Fetching latest rules for ${language} ...`);
					await runGit(['-C', languageRulesCache, 'pull', '--depth=1', '--ff-only']);
				}
			} catch (err) {
				const msg = err instanceof Error ? err.message : String(err);
				const channel = window.createOutputChannel('CWTools');
				channel.appendLine(`[CWTools] Rule fetch failed for ${language}: ${msg}`);
				channel.show(true);
			}
		}

		// If the extension is launched in debug mode then the debug server options are used
		// Otherwise the run options are used
		const serverOptions: ServerOptions = {
			run: { command: serverExe, transport: TransportKind.stdio },
			debug : { command: serverExe, transport: TransportKind.stdio }
		}

		const fileEvents = [
			workspace.createFileSystemWatcher("**/{events,common,map,map_data,prescripted_countries,flags,decisions,missions}/**/*.txt"),
			workspace.createFileSystemWatcher("**/{interface,gfx}/**/*.gui"),
			workspace.createFileSystemWatcher("**/{interface,gfx}/**/*.gfx"),
			workspace.createFileSystemWatcher("**/{interface}/**/*.sfx"),
			workspace.createFileSystemWatcher("**/{interface,gfx,fonts,music,sound}/**/*.asset"),
			workspace.createFileSystemWatcher("**/{localisation,localisation_synced,localization}/**/*.yml"),
			// Watch cached CWT rule files; force posix separators so the glob works on Windows.
			workspace.createFileSystemWatcher(cacheDir.replace(/\\/g, '/') + '/**/*.cwt')
		]

		const clientOptions: LanguageClientOptions = {
			documentSelector: [
				{ scheme: 'file', language: 'paradox' },
				{ scheme: 'file', language: 'stellaris' },
				{ scheme: 'file', language: 'hoi4' },
				{ scheme: 'file', language: 'eu4' },
				{ scheme: 'file', language: 'ck2' },
				{ scheme: 'file', language: 'imperator' },
				{ scheme: 'file', language: 'vic2' },
				{ scheme: 'file', language: 'vic3' },
				{ scheme: 'file', language: 'ck3' },
				{ scheme: 'file', language: 'eu5' }
			],
			synchronize: {
				configurationSection: 'cwtools',
				fileEvents: fileEvents
			},
			initializationOptions: {
				language: language === 'eu5' ? 'paradox' : language,
				isVanillaFolder: isVanillaFolder,
				rulesCache: rulesCacheForServer,
				rules_version: workspace.getConfiguration('cwtools').get('rules_version'),
				repoPath: repoPath,
				diagnosticLogging: workspace.getConfiguration('cwtools').get('logging.diagnostic') },
				revealOutputChannelOn: RevealOutputChannelOn.Error
		}

		const client = new LanguageClient('cwtools', 'Paradox Language Server', serverOptions, clientOptions);
		const log = client.outputChannel
		defaultClient = client;
		client.registerProposedFeatures();
		interface loadingBarParams { enable: boolean; value: string }
		const loadingBarNotification = new NotificationType<loadingBarParams>('loadingBar');
		interface debugStatusBarParams { enable: boolean; value: string }
		const debugStatusBarParamsNotification = new NotificationType<debugStatusBarParams>('debugBar');
		interface CreateVirtualFile { uri: string; fileContent: string }
		const createVirtualFile = new NotificationType<CreateVirtualFile>('createVirtualFile');
		const promptReload = new NotificationType<string>('promptReload')
		const forceReload = new NotificationType<string>('forceReload')
		const promptVanillaPath = new NotificationType<string>('promptVanillaPath')
		interface DidFocusFile { uri : string }
		const didFocusFile = new NotificationType<DidFocusFile>('didFocusFile')
		let status: Disposable | undefined;
		interface UpdateFileList { fileList: FileListItem[] }
		const updateFileList = new NotificationType<UpdateFileList>('updateFileList');

		let latestType : string = '';

		async function didChangeActiveTextEditor(editor : vs.TextEditor | undefined): Promise<void> {
			if (!editor) return;
			const path = editor.document.uri.toString();
			if (languageId == "paradox" && editor.document.languageId == "plaintext") {
				await vs.languages.setTextDocumentLanguage(editor.document, "paradox")
			}
			if (editor.document.languageId == language) {
				await client.sendNotification(didFocusFile, {uri: path});
			}
			const data = await client.sendRequest(
				ExecuteCommandRequest.type,
				{ command: "getFileTypes", arguments: [path] }
			);
			if (data !== undefined && data && data[0]) {
				latestType = data[0];
				await commands.executeCommand('setContext', 'cwtoolsGraphFile', true);
			} else {
				await commands.executeCommand('setContext', 'cwtoolsGraphFile', false);
			}
		}

		context.subscriptions.push(window.onDidChangeActiveTextEditor(didChangeActiveTextEditor));

		if (languageId == "paradox") {
			for (const textDocument of workspace.textDocuments){
				if (textDocument.languageId == "plaintext"){
					await vs.languages.setTextDocumentLanguage(textDocument, "paradox")
				}
			}
		}

		client.onNotification(loadingBarNotification, (param: loadingBarParams) => {
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
		const debugStatusBar = window.createStatusBarItem(vs.StatusBarAlignment.Left);
		context.subscriptions.push(debugStatusBar);
		client.onNotification(debugStatusBarParamsNotification, (param: debugStatusBarParams) => {
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
			if (!game || !fsExistsSync(path.join(directory.fsPath, "common"))) {
				await window.showErrorMessage("The selected folder does not appear to be a supported game folder");
				return;
			}
			const dir = game.subdir ? path.join(directory.fsPath, game.subdir) : directory.fsPath;
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

		if (workspace.name === undefined) {
			await window.showWarningMessage("You have opened a file directly.\n\rFor CWTools to work correctly, the mod folder should be opened using \"File, Open Folder\"")
		}

		let currentGraphDepth = 3;
		const showGraph = async function() {
			const graphData = await getGraphData(latestType, currentGraphDepth);
			const wheelSensitivity : number = workspace.getConfiguration('cwtools.graph').get('zoomSensitivity') ?? 1;
			gp.GraphPanel.create(context.extensionPath);
			gp.GraphPanel.currentPanel!.initialiseGraph(graphData, wheelSensitivity);
		}
		context.subscriptions.push(commands.registerCommand('showGraph', async () => {
			await showGraph();
		}));
		context.subscriptions.push(commands.registerCommand('setGraphDepth', async () => {
			const res = await window.showInputBox(
				{
					placeHolder: "default: 3",
					prompt: "Set graph depth (how many connections to go back from this file)",
					value: currentGraphDepth.toString(),
					validateInput: (v : string) => Number.isInteger(Number(v)) ? undefined : "Please enter a number"
			 });
				if (Number.isInteger(Number(res)))
			{
				currentGraphDepth = Number(res)
				await showGraph()
			}
		}));
		context.subscriptions.push(commands.registerCommand('graphFromJson', async () => {
			const uri = await window.showOpenDialog({filters: {'Json': ['json']}})
			if(!uri){
				return;
			}
			const bytes = await vs.workspace.fs.readFile(uri[0]);
			const data = new TextDecoder('utf-8').decode(bytes);
			const wheelSensitivity: number = workspace.getConfiguration('cwtools.graph').get('zoomSensitivity') ?? 1;
			gp.GraphPanel.create(context.extensionPath);
			gp.GraphPanel.currentPanel!.initialiseGraph(data, wheelSensitivity);
		}));
		// Subscriptions are pushed here so the client is disposed with the extension.
		context.subscriptions.push(new CwtoolsProvider());
		context.subscriptions.push(vs.commands.registerCommand("cwtools.reloadExtension", () =>
			commands.executeCommand('workbench.action.reloadWindow')
		));
		context.subscriptions.push(client);
		try {
			await client.start();
		} catch (err) {
			const msg = err instanceof Error ? err.message : String(err);
			window.showErrorMessage(`CWTools language server failed to start: ${msg}`);
			console.error('[CWTools] client.start() error:', err);
			return;
		}
	}

	let languageId : string;
	const knownLanguageIds = ["stellaris", "eu4", "hoi4", "ck2", "imperator", "vic2", "vic3", "ck3", "eu5"];

	const getLanguageIdFallback = async function() {
		const markerFiles = await workspace.findFiles("**/*.txt", "**/{.git,node_modules,out,dist}/**", 1);
		if (markerFiles.length === 1) {
			const doc = await workspace.openTextDocument(markerFiles[0]);
			if (knownLanguageIds.includes(doc.languageId)) return doc.languageId;
		}
		if (workspace.workspaceFolders && workspace.workspaceFolders.length > 0) {
			return detectFromFolder(workspace.workspaceFolders[0].uri.fsPath, fsExistsSync);
		}
		return null;
	}

	let guessedLanguageId: string | undefined | null = window.activeTextEditor?.document?.languageId;
	if(guessedLanguageId === undefined || !knownLanguageIds.includes(guessedLanguageId)){
		guessedLanguageId = await getLanguageIdFallback();
	}

	switch (guessedLanguageId) {
		case "stellaris": languageId = "stellaris"; break;
		case "eu4": languageId = "eu4"; break;
		case "hoi4": languageId = "hoi4"; break;
		case "ck2": languageId = "ck2"; break;
		case "imperator": languageId = "imperator"; break;
		case "vic2": languageId = "vic2"; break;
		case "vic3": languageId = "vic3"; break;
		case "ck3": languageId = "ck3"; break;
        case "eu5": languageId = "eu5"; break;
		default: languageId = "paradox"; break;
	}
	async function findExeInFiles(gameExeName: string, binariesPrefix = false) {
		if (!workspace.workspaceFolders || workspace.workspaceFolders.length === 0) {
			return [];
		}

		const root = workspace.workspaceFolders[0];
		const isWin = os.platform() === "win32";
		const ext = isWin ? "*.exe" : "*";
		const prefix = binariesPrefix ? "binaries/" : "";
		const names = [gameExeName, gameExeName.toUpperCase(), gameExeName.toLowerCase()];
		const patterns = names.map(name => new vs.RelativePattern(root, `${prefix}${name}${ext}`));

		const results = await Promise.all(patterns.map(p => workspace.findFiles(p)));
		const allFiles = results.flat();
		const validFiles = (await Promise.all(
			allFiles.map(async v => (await exe.existAndIsExe(v.fsPath)) ? v : null)
		)).filter(Boolean);
		return validFiles;
	}
	const games = [
		{ id: "eu4", exeName: "eu4", binariesPrefix: false },
		{ id: "hoi4", exeName: "hoi4", binariesPrefix: false },
		{ id: "stellaris", exeName: "stellaris", binariesPrefix: false },
		{ id: "ck2", exeName: "CK2", binariesPrefix: false },
		{ id: "imperator", exeName: "imperator", binariesPrefix: true },
		{ id: "vic2", exeName: "v2game", binariesPrefix: false },
		{ id: "ck3", exeName: "ck3", binariesPrefix: true },
		{ id: "vic3", exeName: "victoria3", binariesPrefix: true },
        { id: "eu5", exeName: "eu5", binariesPrefix: true },
	];

	const promises = games.map(({ exeName, binariesPrefix }) =>
		findExeInFiles(exeName, binariesPrefix)
	);

	const results = await Promise.all(promises);

	let isVanillaFolder = false;

	for (let i = 0; i < results.length; i++) {
		const { id } = games[i];
		if (results[i].length > 0 && (languageId === "paradox" || languageId === id)) {
			isVanillaFolder = true;
			languageId = id;
		}
	}

	if (
		workspace.workspaceFolders &&
		workspace.workspaceFolders.length > 0 &&
		path.basename(workspace.workspaceFolders[0].uri.fsPath) === "game"
	) {
		isVanillaFolder = true;
	}

	await init(languageId, isVanillaFolder);
}


export async function reloadExtension(prompt: string, buttonText?: string, force? : boolean) {
	const restartAction = buttonText || "Restart";
	const actions = [restartAction];
	if (force) {
		const result = await window.showInformationMessage(prompt, ...actions);
		if(result === restartAction){
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
// export default defaultClient;
