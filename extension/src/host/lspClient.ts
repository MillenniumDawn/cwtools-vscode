import * as path from "path";
import type {
	Disposable,
	ExtensionContext,
	FileSystemWatcher,
	WorkspaceFolder,
} from "vscode";
import {
	CancellationError,
	RelativePattern,
	Uri,
	commands,
	l10n,
	workspace,
	window,
} from "vscode";
import type {
	ErrorHandler,
	LanguageClientOptions,
	ServerOptions,
} from "vscode-languageclient/node";
import { gameContentRoots } from "./gameContentRoots";
import {
	LanguageClient,
	TransportKind,
	RevealOutputChannelOn,
	State,
	ErrorAction,
	CloseAction,
	DidChangeConfigurationNotification,
	ExecuteCommandRequest,
} from "vscode-languageclient/node";
import {
	normalizeBackgroundReindexMinutes,
	normalizeBackgroundReindexIdleSeconds,
	buildSettingsPayload,
	mapIgnoreOptions,
	isLiveSettingsChange,
	isReloadSettingsChange,
	type FormattingIndentStyle,
	type HoverScopeDisplay,
	type LiveServerSettings,
} from "./reindexSettings";
import { DiagnosticsSignatureCache } from "./diagnosticsSignature";
import type { RulesSetup } from "./rulesSetup";
import { createWatchedPathExcluder, forwardWatchedFileEvent } from "./watchedFiles";
import { logError, logInfo, errorMessage, outputChannel } from "./logger";
import {
	isServerCommand,
	serverCommand,
	type ServerCommandName,
} from "../common/serverCommandContract";
import { runCancellableExecuteCommand } from "./commandProgress";

export interface ClientConfig {
	language: string;
	serverExe: string;
	cacheDir: string;
	rulesCache: string;
	resolveRulesCache: () => Promise<RulesSetup>;
	onRulesCacheChanged?: (rulesCache: string) => void;
	fetchRules?: (client: LanguageClient) => void;
	/** The descriptor-bearing root the server must initialize and scan. */
	workspaceFolder: WorkspaceFolder;
}

// Settings-to-server mapping lives in reindexSettings.ts (pure, unit-tested);
// this just reads the raw config values.
function readIgnoreOptions(): {
	ignoreFilePatterns: string[];
	ignoredErrorCodes: string[];
} {
	const cfg = workspace.getConfiguration("cwtools");
	return mapIgnoreOptions(
		cfg.get<string[]>("ignore_patterns"),
		cfg.get<string[]>("errors.ignorefiles"),
		cfg.get<string[]>("errors.ignore"),
	);
}

// Minutes between the server's periodic background re-index passes; the
// server's key (backgroundReindexIntervalMinutes) matches the setting's leaf
// name, just camelCased onto one word.
function readBackgroundReindexMinutes(): number {
	return normalizeBackgroundReindexMinutes(
		workspace
			.getConfiguration("cwtools")
			.get<number>("backgroundReindex.intervalMinutes"),
	);
}

// Idle window the server waits out before starting a background pass.
function readBackgroundReindexIdleSeconds(): number {
	return normalizeBackgroundReindexIdleSeconds(
		workspace
			.getConfiguration("cwtools")
			.get<number>("backgroundReindex.idleSeconds"),
	);
}

function readLiveServerSettings(): LiveServerSettings {
	const cfg = workspace.getConfiguration("cwtools");
	const rawScope = cfg.get<string>("hover.scopeDisplay");
	const hoverScopeDisplay: HoverScopeDisplay =
		rawScope === "resolved" || rawScope === "context" ? rawScope : "context";
	const rawIndent = cfg.get<string>("formatting.indentStyle");
	const formattingIndentStyle: FormattingIndentStyle =
		rawIndent === "tab" || rawIndent === "space" ? rawIndent : "space";
	return {
		localisationLanguages: cfg.get<string[]>("localisation.languages") ?? [
			"English",
		],
		workspaceWideDiagnostics:
			cfg.get<boolean>("diagnostics.workspaceWide") ?? true,
		hoverShowAllLanguages:
			cfg.get<boolean>("localisation.hoverShowAllLanguages") ?? false,
		hoverDebug: cfg.get<boolean>("hover.debug") ?? false,
		hoverScopeDisplay,
		formattingIndentStyle,
		formattingIndentSize: cfg.get<number>("formatting.indentSize") ?? 4,
		formattingMaxLineWidth: cfg.get<number>("formatting.maxLineWidth") ?? 120,
		formattingTrimTrailingWhitespace:
			cfg.get<boolean>("formatting.trimTrailingWhitespace") ?? true,
		formattingInsertFinalNewline:
			cfg.get<boolean>("formatting.insertFinalNewline") ?? true,
	};
}

// Built per call rather than once at module scope: l10n.t is resolved eagerly,
// and the notification title has to be in the language the window is running in.
function commandProgressTitle(
	command: ServerCommandName,
): string | undefined {
	const titles: Partial<Record<ServerCommandName, string>> = {
		[serverCommand("cacheVanilla")]: l10n.t("CWTools: Regenerate game vanilla cache file"),
		[serverCommand("clearAllCaches")]: l10n.t("CWTools: Clear all caches and reindex"),
		[serverCommand("reloadrulesconfig")]: l10n.t("CWTools: Reload config rules"),
		[serverCommand("reindexWorkspace")]: l10n.t("CWTools: Re-index workspace"),
		[serverCommand("validateWorkspace")]: l10n.t("CWTools: Validate workspace"),
	};
	return titles[command];
}

interface WorkspaceValidationSummary {
	totalFiles: number;
	validatedFiles: number;
	heldBackFiles: number;
	filesWithErrors: number;
	totalErrors: number;
	totalWarnings: number;
	totalInfos: number;
	totalHints: number;
}

function workspaceValidationSummary(value: unknown): WorkspaceValidationSummary | undefined {
	if (value === null || typeof value !== "object") {
		return undefined;
	}
	const record = value as Record<string, unknown>;
	const fields = [
		"totalFiles",
		"validatedFiles",
		"heldBackFiles",
		"filesWithErrors",
		"totalErrors",
		"totalWarnings",
		"totalInfos",
		"totalHints",
	] as const;
	if (
		fields.some(
			(field) =>
				typeof record[field] !== "number" ||
				!Number.isSafeInteger(record[field]) ||
				record[field] < 0,
		)
	) {
		return undefined;
	}
	// SAFETY: fields.some above proved all eight fields are non-negative safe integers, the invariant WorkspaceValidationSummary encodes.
	return record as unknown as WorkspaceValidationSummary;
}

function showWorkspaceValidationResult(
	command: ServerCommandName,
	result: unknown,
): void {
	if (result !== null && typeof result === "object") {
		const record = result as Record<string, unknown>;
		if (record.cancelled === true) {
			window.showInformationMessage(l10n.t("CWTools: {0} cancelled.", command));
			return;
		}
		if (record.busy === true) {
			window.showWarningMessage(
				l10n.t("CWTools: workspace validation could not start because another scan is still running. Try again shortly."),
			);
			return;
		}
	}
	const summary = workspaceValidationSummary(result);
	if (summary === undefined) {
		window.showWarningMessage(
			l10n.t("CWTools: workspace validation did not return a summary."),
		);
		return;
	}
	const validatedSummary = l10n.t(
		"CWTools: validated {0} of {1} files; {2} with errors, {3} errors, {4} warnings, {5} infos, {6} hints.",
		summary.validatedFiles,
		summary.totalFiles,
		summary.filesWithErrors,
		summary.totalErrors,
		summary.totalWarnings,
		summary.totalInfos,
		summary.totalHints,
	);
	// Totals cover the whole workspace; Problems lists less with workspace-wide diagnostics off, or past the server's publish budget.
	const workspaceWide =
		workspace
			.getConfiguration("cwtools")
			.get<boolean>("diagnostics.workspaceWide") ?? true;
	let caveat: string | undefined;
	if (!workspaceWide) {
		caveat = l10n.t("Problems only lists open files while workspace-wide diagnostics are off.");
	} else if (summary.heldBackFiles > 0) {
		caveat = l10n.t(
			"Problems leaves out {0} closed files past the workspace diagnostics budget.",
			summary.heldBackFiles,
		);
	}
	const message =
		caveat === undefined ? validatedSummary : `${validatedSummary} ${caveat}`;
	const showProblems = l10n.t("Show Problems");
	void Promise.resolve(window.showInformationMessage(message, showProblems)).then(
		(choice) => {
			if (choice === showProblems) {
				return commands.executeCommand("workbench.actions.view.problems");
			}
			return undefined;
		},
	).catch((err: unknown) =>
		logError("Failed to open the Problems panel after workspace validation", err),
	);
}

// genlocall returns one stub per language; open each as an untitled document so
// the user reviews and saves manually. Paradox loc files require a UTF-8 BOM, so
// prepend it — a manual save then keeps it (VS Code writes the leading U+FEFF as
// the BOM bytes).
async function openGeneratedLoc(result: unknown): Promise<void> {
	const files = Array.isArray(result)
		? (result as Array<{ content?: string }>)
		: [];
	const stubs = files.filter(
		(f) => typeof f.content === "string" && f.content.length > 0,
	);
	if (stubs.length === 0) {
		window.showInformationMessage(
			l10n.t("CWTools: no missing localisation found."),
		);
		return;
	}
	for (const stub of stubs) {
		const content = "\uFEFF" + stub.content;
		const doc = await workspace.openTextDocument({
			content,
			language: "paradox-localisation",
		});
		await window.showTextDocument(doc, { preview: false });
	}
}

// Same restart-limiting shape as the library's own DefaultErrorHandler
// (createDefaultErrorHandler), reimplemented because that one is an instance
// method and errorHandler has to be in clientOptions before the client
// exists. The addition is onStopped, called once restarts give up so the
// status bar item can say so instead of just going quiet.
function createRestartLimitingErrorHandler(
	onStopped: () => void,
): ErrorHandler {
	const maxRestartCount = 4;
	const restarts: number[] = [];
	return {
		// Deliberately unlike DefaultErrorHandler, which shuts down whenever
		// count is undefined. Shutdown calls client.stop(), which puts the client
		// in Stopping — and the library skips the close handler entirely for a
		// stopping client, falling back to a bare DoNotRestart. A crashed server
		// surfaces as an uncounted transport error (the pipe broke), so that
		// branch turned every server panic into a permanent stop on the very
		// first occurrence and the restart budget below never applied (#675).
		// Defer to closed() instead; it owns that budget. Repeated failures on a
		// still-open connection are the one case worth giving up on outright.
		error: (_error, _message, count) =>
			count === undefined || count <= 3
				? { action: ErrorAction.Continue }
				: { action: ErrorAction.Shutdown },
		closed: () => {
			restarts.push(Date.now());
			if (restarts.length <= maxRestartCount) {
				return { action: CloseAction.Restart };
			}
			const diff = restarts[restarts.length - 1] - restarts[0];
			if (diff <= 3 * 60 * 1000) {
				onStopped();
				return {
					action: CloseAction.DoNotRestart,
					message: l10n.t(
						"CWTools: the language server crashed {0} times in the last 3 minutes and won't be restarted. See the output for details.",
						maxRestartCount + 1,
					),
				};
			}
			restarts.shift();
			return { action: CloseAction.Restart };
		},
	};
}

export function createLanguageClient(
	context: ExtensionContext,
	cfg: ClientConfig,
	onStopped: () => void,
): LanguageClient {
	// If the extension is launched in debug mode then the debug server options are used
	// Otherwise the run options are used.
	// When cwtools.profiling is on, launch the server with CWTOOLS_PROFILE=1 so
	// it emits per-phase timing + RSS (to the CWTools output channel) and keeps
	// a buffer the 'Export profiling log' command can save. Takes effect on the
	// next server start, so toggling it needs a reload.
	const profilingEnabled =
		workspace.getConfiguration("cwtools").get<boolean>("profiling") ?? false;
	const serverEnv = profilingEnabled
		? { ...process.env, CWTOOLS_PROFILE: "1" }
		: process.env;
	const serverOptions: ServerOptions = {
		run: {
			command: cfg.serverExe,
			transport: TransportKind.stdio,
			options: { env: serverEnv },
		},
		debug: {
			command: cfg.serverExe,
			transport: TransportKind.stdio,
			options: { env: serverEnv },
		},
	};

	// One watcher per file class the server actually reads, keyed on extension
	// rather than a directory list. Its workspace scan walks the whole tree and
	// filters by SCRIPT_EXTENSIONS (txt, gui, gfx, sfx, asset, map), so a
	// per-directory glob here is always narrower than what it indexes: .txt
	// under gfx/, portraits/ or dlc/ went unwatched. Loc keeps its directory
	// scope, which the server's own loc check requires.
	const fileEvents = [
		workspace.createFileSystemWatcher("**/*.{txt,gui,gfx,sfx,asset,map}"),
		workspace.createFileSystemWatcher(
			"**/{localisation,localisation_synced,localization}/**/*.{yml,yaml,csv}",
		),
	];
	// Preserve structural lint for every workspace .cwt.
	const cwtWatcher = workspace.createFileSystemWatcher("**/*.cwt");
	fileEvents.push(cwtWatcher);
	context.subscriptions.push(...fileEvents);

	let currentRulesCache = cfg.rulesCache;
	let rulesWatcher: FileSystemWatcher | undefined;
	let rulesWatcherSubscriptions: Disposable[] = [];
	let rulesReloadTimer: ReturnType<typeof setTimeout> | undefined;
	let rulesWatcherDisposed = false;
	let rulesWatcherSuspended = false;
	let rulesFolderChangeVersion = 0;
	let configurationUpdatesDisposed = false;
	let configurationUpdateQueue = Promise.resolve();
	const clearRulesReloadTimer = () => {
		if (rulesReloadTimer !== undefined) {
			clearTimeout(rulesReloadTimer);
			rulesReloadTimer = undefined;
		}
	};
	const disposeRulesWatcher = () => {
		clearRulesReloadTimer();
		for (const subscription of rulesWatcherSubscriptions) {
			subscription.dispose();
		}
		rulesWatcherSubscriptions = [];
		rulesWatcher?.dispose();
		rulesWatcher = undefined;
	};
	const scheduleRulesReload = () => {
		if (
			workspace.getConfiguration("cwtools").get<boolean>("rules.autoReload") ===
			false
		) {
			return;
		}
		clearRulesReloadTimer();
		rulesReloadTimer = setTimeout(() => {
			rulesReloadTimer = undefined;
			if (
				rulesWatcherDisposed ||
				workspace
					.getConfiguration("cwtools")
					.get<boolean>("rules.autoReload") === false
			) {
				return;
			}
			client
				.sendRequest(ExecuteCommandRequest.type, {
					command: serverCommand("reloadrulesconfig"),
					arguments: [],
				})
				.catch((err) => logError("Automatic rules reload failed", err));
		}, 500);
	};
	// Returns whether this call installed a watcher.
	const installRulesWatcher = (): boolean => {
		if (
			rulesWatcherDisposed ||
			rulesWatcherSuspended ||
			rulesWatcherSubscriptions.length > 0
		)
			return false;
		if (workspace.getWorkspaceFolder(Uri.file(currentRulesCache))) {
			const reloadSelectedRules = (uri: Uri) => {
				const relative = path.relative(currentRulesCache, uri.fsPath);
				if (
					relative === ".." ||
					relative.startsWith(`..${path.sep}`) ||
					path.isAbsolute(relative)
				)
					return;
				scheduleRulesReload();
			};
			rulesWatcherSubscriptions = [
				cwtWatcher.onDidCreate(reloadSelectedRules),
				cwtWatcher.onDidChange(reloadSelectedRules),
				cwtWatcher.onDidDelete(reloadSelectedRules),
			];
			return true;
		}
		const watcher = workspace.createFileSystemWatcher(
			new RelativePattern(Uri.file(currentRulesCache), "**/*.cwt"),
		);
		rulesWatcher = watcher;
		const watchRulesFileChange = (uri: Uri, type: 1 | 2 | 3) => {
			client
				.sendNotification("workspace/didChangeWatchedFiles", {
					changes: [{ uri: uri.toString(), type }],
				})
				.catch((err) =>
					logError("Failed to forward a rules-file change to the server", err),
				);
			scheduleRulesReload();
		};
		rulesWatcherSubscriptions = [
			watcher.onDidCreate((uri) => watchRulesFileChange(uri, 1)),
			watcher.onDidChange((uri) => watchRulesFileChange(uri, 2)),
			watcher.onDidDelete((uri) => watchRulesFileChange(uri, 3)),
		];
		return true;
	};
	const rulesWatcherLifetime: Disposable = {
		dispose: () => {
			rulesWatcherDisposed = true;
			configurationUpdatesDisposed = true;
			rulesFolderChangeVersion++;
			disposeRulesWatcher();
		},
	};
	const changeRulesCache = (rulesCache: string) => {
		if (rulesCache === currentRulesCache) return;
		disposeRulesWatcher();
		currentRulesCache = rulesCache;
		isExcludedWatchedPath = createWatchedPathExcluder(startupRoots, currentRulesCache);
		cfg.onRulesCacheChanged?.(rulesCache);
		if (installRulesWatcher()) {
			logInfo(`Watching rules in ${rulesCache}`);
		}
	};

	const diagnosticsCache = new DiagnosticsSignatureCache();
	const workspaceRoot = cfg.workspaceFolder.uri.fsPath;
	const readStartupRoots = () => {
		const settings = workspace.getConfiguration("cwtools");
		return gameContentRoots(
			workspaceRoot,
			settings.get<string[]>("parentMods"),
			settings.get<string>("cache." + cfg.language),
		);
	};
	let startupRoots = readStartupRoots();
	let isExcludedWatchedPath = createWatchedPathExcluder(startupRoots, currentRulesCache);


	const middleware: LanguageClientOptions["middleware"] = {
		workspace: {
			// Extension-keyed globs also catch files the server's own discovery
			// walk skips, and its watched-file path doesn't re-apply that skip
			// list, so hold those events here.
			didChangeWatchedFile: async (event, next) => {
				await forwardWatchedFileEvent(
					event,
					isExcludedWatchedPath,
					(uri) => Uri.parse(uri).fsPath,
					next,
				);
			},
		},
		handleDiagnostics: (uri, diagnostics, next) => {
			if (diagnosticsCache.shouldPublish(uri.toString(), diagnostics)) {
				next(uri, diagnostics);
			}
		},
		executeCommand: async (command, args, next) => {
			// getGraphData returns the graph the webview renders, so its result
			// isn't a toast and — unlike the commands below — a failure has to
			// reach the caller instead of becoming an error message and an
			// `undefined` the panel would try to draw. `serverProgress: false`
			// keeps Cancel on the `$/cancelRequest` path: the server has no
			// graceful cancel for this one, so a token would give the
			// notification a Cancel button that stops nothing.
			if (command === serverCommand("getGraphData")) {
				return await runCancellableExecuteCommand(
					client,
					command,
					args,
					l10n.t("CWTools: Build graph"),
					{ serverProgress: false },
				);
			}
			// genlocall returns generated loc stubs to open, not a toast string.
			if (command === serverCommand("genlocall")) {
				try {
					const result = await runCancellableExecuteCommand(
						client,
						command,
						args,
						l10n.t("CWTools: Generate missing loc for all files"),
						// One synchronous sweep server-side with no cancel seam,
						// so Cancel stays the `$/cancelRequest` fallback rather
						// than a notification the server would ignore.
						{ serverProgress: false },
					);
					await openGeneratedLoc(result);
					return result;
				} catch (err) {
					if (err instanceof CancellationError) {
						window.showInformationMessage(
							l10n.t("CWTools: genlocall cancelled."),
						);
						return undefined;
					}
					const msg = errorMessage(err);
					window.showErrorMessage(
						l10n.t("CWTools: genlocall failed: {0}", msg),
					);
					return undefined;
				}
			}
			if (!isServerCommand(command)) {
				const result: unknown = await next(command, args);
				return result;
			}
			const title = commandProgressTitle(command);
			if (title === undefined) {
				const result: unknown = await next(command, args);
				return result;
			}
			try {
				const result = await runCancellableExecuteCommand(
					client,
					command,
					args,
					title,
				);
				if (command === serverCommand("validateWorkspace")) {
					showWorkspaceValidationResult(command, result);
					return result;
				}
				// Against a server that supports command progress this covers
				// cancellation too: the command returns normally and says so
				// ("Re-index cancelled.") instead of being dropped mid-flight.
				if (typeof result === "string" && result.length > 0) {
					window.showInformationMessage(`CWTools: ${result}`);
				}
				return result;
			} catch (err) {
				if (err instanceof CancellationError) {
					// The `$/cancelRequest` fallback, where the handler was dropped
					// and there is no server reply to report. Say so anyway — a
					// notification that just vanishes reads as a silent failure.
					window.showInformationMessage(
						l10n.t("CWTools: {0} cancelled.", command),
					);
					return undefined;
				}
				const msg = errorMessage(err);
				window.showErrorMessage(
					l10n.t("CWTools: {0} failed: {1}", command, msg),
				);
				return undefined;
			}
		},
	};

	const clientOptions: LanguageClientOptions = {
		documentSelector: [
			{ scheme: "file", language: "paradox" },
			// .cwt rule-config files: the server lints them structurally
			// (undefined type/enum/single_alias refs + parse errors) rather
			// than running the game-script validator. See cwtools-vscode#43.
			{ scheme: "file", language: "cwt" },
			// Localisation .yml files: under a localisation* folder they open as
			// our dedicated 'paradox-localisation' language (Paradox loc is not
			// real YAML: strings run to the last quote, KEY:0 version suffixes,
			// embedded [cmd]/$ref$/§colour/£icon). The server routes loc by path,
			// not language id, so it attaches the same. The 'yaml'+pattern entry
			// stays as a fallback for any loc file VS Code still opens as YAML.
			{ scheme: "file", language: "paradox-localisation" },
			{
				scheme: "file",
				language: "yaml",
				pattern:
					"**/{localisation,localisation_synced,localization}/**/*.{yml,yaml,csv}",
			},
		],
		synchronize: {
			// The `cwtools.*` settings use different names than the server's init
			// options (e.g. errors.ignore vs ignoredErrorCodes), so the library's
			// raw-section push would never deliver the mapped keys. We push the
			// mapped payload ourselves on change instead (see below).
			fileEvents: fileEvents,
		},
		initializationOptions: () => {
			startupRoots = readStartupRoots();
			isExcludedWatchedPath = createWatchedPathExcluder(startupRoots, currentRulesCache);
			const ignoreOptions = readIgnoreOptions();
			return {
				language: cfg.language === "eu5" ? "paradox" : cfg.language,
				rulesCache: currentRulesCache,
				...readLiveServerSettings(),
				// Inlay hints. The server reads both at initialize only — neither key is
				// in its didChangeConfiguration handler — so a change needs a window
				// reload, which the setting descriptions say.
				inlayHintsLocTitles:
					workspace
						.getConfiguration("cwtools")
						.get<boolean>("inlayHints.locTitles") ?? true,
				inlayHintsScopes:
					workspace
						.getConfiguration("cwtools")
						.get<boolean>("inlayHints.scopes") ?? false,
				// Persistent cache dir + the user's vanilla install path. The Rust
				// server caches the base-game index here keyed by game version, so
				// it isn't re-parsed every startup. Passing the explicit install
				// path avoids relying on Steam auto-discovery.
				cacheDir: path.join(cfg.cacheDir, "vanilla"),
				vanilla: workspace
					.getConfiguration("cwtools")
					.get("cache." + cfg.language),
				// Parent mods of a submod, in load order (#786). The server indexes
				// them between the base game and the workspace at startup only.
				parentMods:
					workspace
						.getConfiguration("cwtools")
						.get<string[]>("parentMods") ?? [],
				ignoreFilePatterns: ignoreOptions.ignoreFilePatterns,
				ignoredErrorCodes: ignoreOptions.ignoredErrorCodes,
				backgroundReindexIntervalMinutes: readBackgroundReindexMinutes(),
				backgroundReindexIdleSeconds: readBackgroundReindexIdleSeconds(),
			};
		},
		// Never force-reveal: genuine failures still surface via window.showErrorMessage in extension.ts.
		revealOutputChannelOn: RevealOutputChannelOn.Never,
		// Without this the client opens its own channel and the server's
		// window/logMessage output never reaches the one users are sent to.
		outputChannel,
		// The server advertises its commands (cacheVanilla, clearAllCaches,
		// reloadrulesconfig, genlocall, ...) in executeCommandProvider, and
		// vscode-languageclient registers each as a VS Code command. Registering
		// them ourselves too makes client.start() throw "command already exists",
		// so the UX (result toasts, opening the generated loc) lives here instead.
		middleware,
		// Lock the client to the same descriptor-bearing root selected by
		// activation. This makes initialize.rootUri/workspaceFolders contain only
		// that root, so the server's first-root scan cannot drift to another folder.
		workspaceFolder: cfg.workspaceFolder,
		errorHandler: createRestartLimitingErrorHandler(onStopped),
	};

	const client = new LanguageClient(
		"cwtools",
		"Paradox Language Server",
		serverOptions,
		clientOptions,
	);
	installRulesWatcher();
	context.subscriptions.push(rulesWatcherLifetime);

	// Client clears the DiagnosticCollection on stop; drop the cache too or the
	// re-publish after restart looks unchanged and squiggles don't return.
	context.subscriptions.push(
		// onDidChangeState is a lib getter returning Event<StateChangeEvent>;
		// type-aware lint resolves it as unsafe under skipLibCheck though tsc
		// types it (client, the handler and the returned Disposable are typed).
		// eslint-disable-next-line @typescript-eslint/no-unsafe-call, @typescript-eslint/no-unsafe-argument
		client.onDidChangeState((e: { oldState: State; newState: State }) => {
			if (e.oldState === State.Running) {
				diagnosticsCache.clear();
			}
			if (e.newState === State.Stopped) {
				rulesWatcherSuspended = true;
				disposeRulesWatcher();
			} else if (e.newState === State.Starting) {
				rulesWatcherSuspended = false;
				installRulesWatcher();
			}
		}),
	);

	// Push mapped configuration when a live setting changes. We drive this
	// ourselves rather than via synchronize.configurationSection, which would
	// send the raw (unmapped) `cwtools` section the server can't read.
	// The allow-lists live in reindexSettings.ts so a new setting can't be added
	// in one place and forgotten in the other.
	context.subscriptions.push(
		workspace.onDidChangeConfiguration((e) => {
			if (isReloadSettingsChange(e)) {
				const reloadWindow = l10n.t("Reload Window");
				void Promise.resolve(
					window.showInformationMessage(
						l10n.t(
							"CWTools settings changed. Reload the window to apply them.",
						),
						reloadWindow,
					),
				)
					.then((action) => {
						if (action === reloadWindow) {
							return commands.executeCommand("workbench.action.reloadWindow");
						}
						return undefined;
					})
					.catch((err: unknown) =>
						logError("Failed to reload window after settings change", err),
					);
			}
			const rulesFolderChanged = e.affectsConfiguration("cwtools.rules_folder");
			if (!isLiveSettingsChange(e) && !rulesFolderChanged) {
				return;
			}
			const version = rulesFolderChanged
				? ++rulesFolderChangeVersion
				: rulesFolderChangeVersion;
			const updateSettings = async () => {
				const rulesSetup = rulesFolderChanged
					? await cfg.resolveRulesCache()
					: undefined;
				if (
					configurationUpdatesDisposed ||
					(rulesFolderChanged && version !== rulesFolderChangeVersion)
				)
					return;
				const settings = {
					...buildSettingsPayload(
						readIgnoreOptions(),
						readBackgroundReindexMinutes(),
						readBackgroundReindexIdleSeconds(),
						readLiveServerSettings(),
					),
					...(rulesSetup ? { rulesCache: rulesSetup.rulesCache } : {}),
				};
				await client.sendNotification(DidChangeConfigurationNotification.type, {
					settings,
				});
				if (
					!configurationUpdatesDisposed &&
					rulesSetup &&
					version === rulesFolderChangeVersion
				) {
					changeRulesCache(rulesSetup.rulesCache);
					if (rulesSetup.fetchUpstream) cfg.fetchRules?.(client);
				}
			};
			configurationUpdateQueue = configurationUpdateQueue
				.then(updateSettings)
				.catch((err: unknown) =>
					logError("Failed to push updated settings to the server", err),
				);
		}),
	);

	return client;
}
