/* --------------------------------------------------------------------------------------------
 * Copyright (c) Microsoft Corporation. All rights reserved.
 * Licensed under the MIT License. See License.txt in the project root for license information.
 * ------------------------------------------------------------------------------------------ */

import * as path from 'path';
import * as os from 'os';
import { existsSync as fsExistsSync } from 'fs';
import * as fsPromises from 'fs/promises';
import * as vscode from 'vscode';
import type { ExtensionContext, Disposable} from 'vscode';
import { workspace, window, Uri, WorkspaceEdit, TextEdit, Range, commands, env } from 'vscode';
import type { LanguageClientOptions, ServerOptions} from 'vscode-languageclient/node';
import { LanguageClient, TransportKind, NotificationType, ExecuteCommandRequest, RevealOutputChannelOn } from 'vscode-languageclient/node';

import type { FileListItem } from './fileExplorer';
import { FileExplorer } from './fileExplorer';
import { getGraphData } from '../common/graphTypes';
import {
	LANGUAGE_REPOS,
	GAME_DISPLAY,
	GAME_FOLDER,
	resolveRulesFolder,
	serverExe as resolveServerExe,
	runGit,
} from './engine';
import { detectGameAndVanilla } from './detectGame';
import { logInfo, logWarn, logError } from './logger';

interface LoadingBarParams { enable: boolean; value: string }
interface DebugStatusBarParams { enable: boolean; value: string }
interface CreateVirtualFile { uri: string; fileContent: string }
interface DidFocusFile { uri: string }
interface UpdateFileList { fileList: FileListItem[] }

export let defaultClient: LanguageClient;
export async function activate(context: ExtensionContext) {
	let fileList : FileListItem[];
	let fileExplorer : FileExplorer;


	class CwtoolsProvider implements vscode.TextDocumentContentProvider
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

	// Writable, per-extension cache dir. globalStorage survives extension
	// updates and is writable everywhere; the install dir (extensionPath) is
	// wiped on every update and can be read-only.
	const cacheDir = path.join(context.globalStorageUri.fsPath, '.cwtools')

	const init = async function(language : string, isVanillaFolder : boolean) {
		const langConfigDisposable = vscode.languages.setLanguageConfiguration(language, { wordPattern : /"?([^\s.]+)"?/ });
		context.subscriptions.push(langConfigDisposable);

		// The Rust language server, bundled per-platform. Resolve the binary for
		// this platform or bail with a clear message.
		const serverExe = resolveServerExe(context);
		if (!serverExe) {
			await window.showErrorMessage(
				`CWTools: no language server binary found. ` +
				`Re-install the extension or build the server.`);
			return;
		}
		logInfo(`Using server: ${serverExe}`);

	if (os.platform() !== 'win32') {
		try {
			const stat = await fsPromises.stat(serverExe);
			if ((stat.mode & 0o111) === 0) {
				await fsPromises.chmod(serverExe, 0o755);
			}
		} catch (e) {
			logError('stat/chmod error on server binary', e);
		}
	}

		const repoPath = LANGUAGE_REPOS[language];
		if (!repoPath) {
			logWarn(`No config repository for language "${language}"; rule cloning skipped.`);
		}
		logInfo(`${language} ${repoPath || '(no remote)'}`);

		await fsPromises.mkdir(cacheDir, { recursive: true });
		const languageRulesCache = path.join(cacheDir, language);

		const rawManualRules = workspace.getConfiguration('cwtools').get<string>('rules_folder');
		const workspaceRoot = workspace.workspaceFolders?.[0]?.uri.fsPath;
		const manualRules = resolveRulesFolder(rawManualRules, { workspaceRoot });
		const hasManualRules = !!(rawManualRules && rawManualRules.trim() !== '');
		const effectiveRulesCache = manualRules.existed ? manualRules.path! : languageRulesCache;
		const rulesCacheForServer = effectiveRulesCache;
		if (manualRules.existed) {
			logInfo(`Using manual rules folder: ${manualRules.path}`);
		} else if (hasManualRules) {
			// rules_folder is set but unusable. Warn loudly instead of silently
			// cloning upstream, so the user knows their local rules were ignored.
			logWarn(`rules_folder "${rawManualRules}" does not exist (tried "${manualRules.path}"); falling back to bundled/upstream rules.`);
			void window.showWarningMessage(
				`CWTools: the rules_folder "${rawManualRules}" could not be found (tried "${manualRules.path}"). Falling back to the bundled/upstream rules.`
			);
		}
		if (!manualRules.existed && repoPath) {
			try {
				const gitDir = path.join(languageRulesCache, '.git');
				if (!fsExistsSync(gitDir)) {
					logInfo(`Cloning rules from ${repoPath} into ${languageRulesCache}`);
					await runGit(['clone', '--depth', '1', repoPath, languageRulesCache]);
				} else {
					logInfo(`Fetching latest rules for ${language} ...`);
					await runGit(['-C', languageRulesCache, 'pull', '--depth=1', '--ff-only']);
				}
			} catch (err) {
				const msg = err instanceof Error ? err.message : String(err);
				logError(`Rule fetch failed for ${language}`, msg);
			}
		}

		// If the extension is launched in debug mode then the debug server options are used
		// Otherwise the run options are used.
		// When cwtools.profiling is on, launch the server with CWTOOLS_PROFILE=1 so
		// it emits per-phase timing + RSS (to the CWTools output channel) and keeps
		// a buffer the 'Export profiling log' command can save. Takes effect on the
		// next server start, so toggling it needs a reload.
		const profilingEnabled = workspace.getConfiguration('cwtools').get<boolean>('profiling') ?? false;
		const serverEnv = profilingEnabled ? { ...process.env, CWTOOLS_PROFILE: '1' } : process.env;
		const serverOptions: ServerOptions = {
			run: { command: serverExe, transport: TransportKind.stdio, options: { env: serverEnv } },
			debug : { command: serverExe, transport: TransportKind.stdio, options: { env: serverEnv } }
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
		context.subscriptions.push(...fileEvents);

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
				{ scheme: 'file', language: 'eu5' },
				// Localisation .yml files open as the built-in 'yaml' language, so
				// they never matched a game-language selector and the server never
				// saw them (no loc-key completion / hover / goto). Match them by
				// path so YAML highlighting is preserved and unrelated yaml files
				// stay untouched.
				{ scheme: 'file', language: 'yaml', pattern: '**/{localisation,localisation_synced,localization}/**/*.yml' }
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
				localisationLanguages: workspace.getConfiguration('cwtools').get('localisation.languages'),
				hoverShowAllLanguages: workspace.getConfiguration('cwtools').get('localisation.hoverShowAllLanguages') ?? false,
					hoverDebug: workspace.getConfiguration('cwtools').get('hover.debug') ?? false,
				// Persistent cache dir + the user's vanilla install path. The Rust
				// server caches the base-game index here keyed by game version, so
				// it isn't re-parsed every startup. Passing the explicit install
				// path avoids relying on Steam auto-discovery.
				cacheDir: path.join(cacheDir, 'vanilla'),
				vanilla: workspace.getConfiguration('cwtools').get('cache.' + language),
				diagnosticLogging: workspace.getConfiguration('cwtools').get('logging.diagnostic') },
				revealOutputChannelOn: RevealOutputChannelOn.Error,
			// The server advertises its commands (cacheVanilla, clearAllCaches, ...)
			// in executeCommandProvider, and vscode-languageclient registers each as
			// a VS Code command. Registering them ourselves too makes client.start()
			// throw "command already exists", so the toast UX lives here instead.
			middleware: {
				executeCommand: async (command, args, next) => {
					const isCacheCommand = command === 'cacheVanilla' || command === 'clearAllCaches';
					if (!isCacheCommand) {
						return next(command, args);
					}
					try {
						const result = await next(command, args);
						if (typeof result === 'string' && result.length > 0) {
							window.showInformationMessage(`CWTools: ${result}`);
						}
						return result;
					} catch (err) {
						const msg = err instanceof Error ? err.message : String(err);
						window.showErrorMessage(`CWTools: ${command} failed: ${msg}`);
						return undefined;
					}
				}
			}
		}

		const client = new LanguageClient('cwtools', 'Paradox Language Server', serverOptions, clientOptions);
		const log = client.outputChannel
		defaultClient = client;
		client.registerProposedFeatures();
		const loadingBarNotification = new NotificationType<LoadingBarParams>('loadingBar');
		const debugStatusBarParamsNotification = new NotificationType<DebugStatusBarParams>('debugBar');
		const createVirtualFile = new NotificationType<CreateVirtualFile>('createVirtualFile');
		const promptReload = new NotificationType<string>('promptReload')
		const forceReload = new NotificationType<string>('forceReload')
		const promptVanillaPath = new NotificationType<string>('promptVanillaPath')
		const didFocusFile = new NotificationType<DidFocusFile>('didFocusFile')
		let status: Disposable | undefined;
		const updateFileList = new NotificationType<UpdateFileList>('updateFileList');

	let latestType : string = '';
	let getFileTypesInFlight = false;
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
		if (editor.document.languageId === language) {
			await client.sendNotification(didFocusFile, {uri: editorPath});
		}
		// Guard against rapid tab switches piling up requests to a busy server.
		// Only one getFileTypes request can be in flight at a time; subsequent
		// tab switches are skipped until the in-flight request completes or times out.
		if (getFileTypesInFlight) return;
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
	}

		context.subscriptions.push(window.onDidChangeActiveTextEditor(didChangeActiveTextEditor));

		for (const textDocument of workspace.textDocuments){
			await upgradePlaintextDocument(textDocument)
		}
		context.subscriptions.push(workspace.onDidOpenTextDocument(upgradePlaintextDocument));

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

		if (workspace.name === undefined) {
			await window.showWarningMessage("You have opened a file directly.\n\rFor CWTools to work correctly, the mod folder should be opened using \"File, Open Folder\"")
		}

		let currentGraphDepth = 3;
		const wheelSensitivity = (): number => workspace.getConfiguration('cwtools.graph').get('zoomSensitivity') ?? 1;
		const showGraph = async function() {
			const [gp, graphData] = await Promise.all([
				import('./graphPanel'),
				getGraphData(latestType, currentGraphDepth),
			]);
			gp.GraphPanel.create(context.extensionPath);
			gp.GraphPanel.currentPanel!.initialiseGraph(graphData, wheelSensitivity());
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
			const bytes = await vscode.workspace.fs.readFile(uri[0]);
			const data = new TextDecoder('utf-8').decode(bytes);
			const gp = await import('./graphPanel');
			gp.GraphPanel.create(context.extensionPath);
			gp.GraphPanel.currentPanel!.initialiseGraph(data, wheelSensitivity());
		}));
		// cacheVanilla / clearAllCaches are NOT registered here: the language
		// client registers them from the server's executeCommandProvider, and
		// the executeCommand middleware above surfaces their results.

		// Fetch the server's accumulated profiling report and save it to a file.
		// The server only fills the buffer when launched with CWTOOLS_PROFILE=1
		// (the cwtools.profiling setting), so prompt to enable it if empty.
		context.subscriptions.push(commands.registerCommand('cwtools.exportProfilingLog', async () => {
			let log: unknown;
			try {
				log = await client.sendRequest(ExecuteCommandRequest.type, { command: 'exportProfilingLog', arguments: [] });
			} catch (err) {
				const msg = err instanceof Error ? err.message : String(err);
				window.showErrorMessage(`CWTools: could not fetch profiling log: ${msg}`);
				return;
			}
			if (typeof log !== 'string' || log.length === 0) {
				window.showWarningMessage("CWTools: profiling log is empty. Turn on 'cwtools.profiling', reload the window, reproduce the slowdown, then export.");
				return;
			}
			const uri = await window.showSaveDialog({ filters: { 'Log': ['log', 'txt'] }, saveLabel: 'Export CWTools profiling log' });
			if (!uri) { return; }
			await workspace.fs.writeFile(uri, Buffer.from(log, 'utf8'));
			window.showInformationMessage(`CWTools: profiling log written to ${uri.fsPath}`);
		}));
		// Subscriptions are pushed here so the client is disposed with the extension.
		context.subscriptions.push(new CwtoolsProvider());
		context.subscriptions.push(vscode.commands.registerCommand("cwtools.reloadExtension", () =>
			commands.executeCommand('workbench.action.reloadWindow')
		));
		context.subscriptions.push(client);
		try {
			await client.start();
		} catch (err) {
			const msg = err instanceof Error ? err.message : String(err);
			logError('client.start() error', err);
			// EPERM/EACCES means the OS refused to execute the server binary,
			// almost always antivirus (Defender) quarantining the unsigned exe
			// or a corporate exec policy. A raw "spawn EPERM" tells a modder
			// nothing, so surface the cause and a self-serve fix instead.
			const code = (err as NodeJS.ErrnoException | undefined)?.code;
			if (code === 'EPERM' || code === 'EACCES') {
				const reveal = 'Reveal Server Binary';
				const help = 'Antivirus Help';
				void window.showErrorMessage(
					`CWTools server was blocked from running (${code}). This is almost always ` +
					`antivirus (e.g. Windows Defender) quarantining the unsigned server binary. ` +
					`Restore it from quarantine and add an exclusion for the extension's server folder, ` +
					`then reload the window.`,
					reveal, help
				).then(choice => {
					if (choice === reveal && serverExe) {
						void commands.executeCommand('revealFileInOS', Uri.file(serverExe));
					} else if (choice === help) {
						void env.openExternal(Uri.parse(
							'https://support.microsoft.com/windows/add-an-exclusion-to-windows-security-811816c0-4dfd-af4a-47e4-c301afe13b26'));
					}
				});
				return;
			}
			window.showErrorMessage(`CWTools language server failed to start: ${msg}`);
			return;
		}
	}

	const { languageId, isVanillaFolder } = await detectGameAndVanilla();

	await init(languageId, isVanillaFolder);
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
