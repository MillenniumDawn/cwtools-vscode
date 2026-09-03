import * as path from "path";
import type { ExtensionContext } from "vscode";
import { CancellationError, Uri, l10n, workspace, window } from "vscode";
import type {
	ErrorHandler,
	LanguageClientOptions,
	ServerOptions,
} from "vscode-languageclient/node";
import {
	LanguageClient,
	TransportKind,
	RevealOutputChannelOn,
	State,
	ErrorAction,
	CloseAction,
	DidChangeConfigurationNotification,
} from "vscode-languageclient/node";
import {
	normalizeBackgroundReindexMinutes,
	normalizeBackgroundReindexIdleSeconds,
	buildSettingsPayload,
	mapIgnoreOptions,
	isLiveSettingsChange,
	type FormattingIndentStyle,
	type HoverScopeDisplay,
	type LiveServerSettings,
} from "./reindexSettings";
import { DiagnosticsSignatureCache } from "./diagnosticsSignature";
import { isExcludedWatchedPath } from "./watchedFiles";
import { logError, errorMessage, outputChannel } from "./logger";
import { runCancellableExecuteCommand } from "./commandProgress";

export interface ClientConfig {
	language: string;
	serverExe: string;
	cacheDir: string;
	rulesCache: string;
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
		hoverShowAllLanguages:
			cfg.get<boolean>("localisation.hoverShowAllLanguages") ?? false,
		hoverDebug: cfg.get<boolean>("hover.debug") ?? false,
		hoverScopeDisplay,
		formattingIndentStyle,
		formattingIndentSize: cfg.get<number>("formatting.indentSize") ?? 4,
		formattingTrimTrailingWhitespace:
			cfg.get<boolean>("formatting.trimTrailingWhitespace") ?? true,
		formattingInsertFinalNewline:
			cfg.get<boolean>("formatting.insertFinalNewline") ?? true,
	};
}

// Built per call rather than once at module scope: l10n.t is resolved eagerly,
// and the notification title has to be in the language the window is running in.
function commandProgressTitle(command: string): string | undefined {
	const titles: Readonly<Record<string, string>> = {
		cacheVanilla: l10n.t("CWTools: Regenerate game vanilla cache file"),
		clearAllCaches: l10n.t("CWTools: Clear all caches and reindex"),
		reloadrulesconfig: l10n.t("CWTools: Reload config rules"),
		reindexWorkspace: l10n.t("CWTools: Re-index workspace"),
	};
	return titles[command];
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
		error: (_error, _message, count) =>
			count !== undefined && count > 0 && count <= 3
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
		// .cwt rule files: the server lints them and builds its ruleset from
		// them, so an edit made outside the editor (git checkout, another tool)
		// is otherwise invisible until the user runs reloadrulesconfig.
		workspace.createFileSystemWatcher("**/*.cwt"),
	];
	context.subscriptions.push(...fileEvents);

	const diagnosticsCache = new DiagnosticsSignatureCache();

	const middleware: LanguageClientOptions["middleware"] = {
		workspace: {
			// Extension-keyed globs also catch files the server's own discovery
			// walk skips, and its watched-file path doesn't re-apply that skip
			// list, so hold those events here.
			didChangeWatchedFile: async (event, next) => {
				if (isExcludedWatchedPath(Uri.parse(event.uri).fsPath)) {
					return;
				}
				await next(event);
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
			if (command === "getGraphData") {
				return await runCancellableExecuteCommand(
					client,
					command,
					args,
					l10n.t("CWTools: Build graph"),
					{ serverProgress: false },
				);
			}
			// genlocall returns generated loc stubs to open, not a toast string.
			if (command === "genlocall") {
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
			const ignoreOptions = readIgnoreOptions();
			return {
				language: cfg.language === "eu5" ? "paradox" : cfg.language,
				rulesCache: cfg.rulesCache,
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
		errorHandler: createRestartLimitingErrorHandler(onStopped),
	};

	const client = new LanguageClient(
		"cwtools",
		"Paradox Language Server",
		serverOptions,
		clientOptions,
	);

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
		}),
	);

	// Push mapped configuration when a live setting changes. We drive this
	// ourselves rather than via synchronize.configurationSection, which would
	// send the raw (unmapped) `cwtools` section the server can't read.
	// The allow-list lives in reindexSettings.LIVE_SETTINGS_KEYS so a new
	// live key can't be added in one place and forgotten in the other.
	context.subscriptions.push(
		workspace.onDidChangeConfiguration((e) => {
			if (!isLiveSettingsChange(e)) {
				return;
			}
			const settings = buildSettingsPayload(
				readIgnoreOptions(),
				readBackgroundReindexMinutes(),
				readBackgroundReindexIdleSeconds(),
				readLiveServerSettings(),
			);
			client
				.sendNotification(DidChangeConfigurationNotification.type, { settings })
				.catch((err) =>
					logError("Failed to push updated settings to the server", err),
				);
		}),
	);

	return client;
}
