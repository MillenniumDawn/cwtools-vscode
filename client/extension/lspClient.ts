import * as path from 'path';
import type { ExtensionContext } from 'vscode';
import { workspace, window } from 'vscode';
import type { LanguageClientOptions, ServerOptions } from 'vscode-languageclient/node';
import { LanguageClient, TransportKind, RevealOutputChannelOn, DidChangeConfigurationNotification, State } from 'vscode-languageclient/node';
import { normalizeBackgroundReindexMinutes, normalizeBackgroundReindexIdleSeconds, buildReindexSettingsPayload, mapIgnoreOptions } from './reindexSettings';
import { DiagnosticsSignatureCache } from './diagnosticsSignature';
import { logError, outputChannel } from './logger';

export interface ClientConfig {
	language: string;
	serverExe: string;
	cacheDir: string;
	rulesCache: string;
}

// Settings-to-server mapping lives in reindexSettings.ts (pure, unit-tested);
// this just reads the raw config values.
function readIgnoreOptions(): { ignoreFilePatterns: string[]; ignoredErrorCodes: string[] } {
	const cfg = workspace.getConfiguration('cwtools');
	return mapIgnoreOptions(
		cfg.get<string[]>('ignore_patterns'),
		cfg.get<string[]>('errors.ignorefiles'),
		cfg.get<string[]>('errors.ignore'),
	);
}

// Minutes between the server's periodic background re-index passes; the
// server's key (backgroundReindexIntervalMinutes) matches the setting's leaf
// name, just camelCased onto one word.
function readBackgroundReindexMinutes(): number {
	return normalizeBackgroundReindexMinutes(workspace.getConfiguration('cwtools').get<number>('backgroundReindex.intervalMinutes'));
}

// Idle window the server waits out before starting a background pass.
function readBackgroundReindexIdleSeconds(): number {
	return normalizeBackgroundReindexIdleSeconds(workspace.getConfiguration('cwtools').get<number>('backgroundReindex.idleSeconds'));
}

// genlocall returns one stub per language; open each as an untitled document so
// the user reviews and saves manually. Paradox loc files require a UTF-8 BOM, so
// prepend it — a manual save then keeps it (VS Code writes the leading U+FEFF as
// the BOM bytes).
async function openGeneratedLoc(result: unknown): Promise<void> {
	const files = Array.isArray(result) ? result as Array<{ content?: string }> : [];
	const stubs = files.filter(f => typeof f.content === 'string' && f.content.length > 0);
	if (stubs.length === 0) {
		window.showInformationMessage('CWTools: no missing localisation found.');
		return;
	}
	for (const stub of stubs) {
		const content = '\uFEFF' + stub.content;
		const doc = await workspace.openTextDocument({ content, language: 'paradox-localisation' });
		await window.showTextDocument(doc, { preview: false });
	}
}

export function createLanguageClient(context: ExtensionContext, cfg: ClientConfig): LanguageClient {
	// If the extension is launched in debug mode then the debug server options are used
	// Otherwise the run options are used.
	// When cwtools.profiling is on, launch the server with CWTOOLS_PROFILE=1 so
	// it emits per-phase timing + RSS (to the CWTools output channel) and keeps
	// a buffer the 'Export profiling log' command can save. Takes effect on the
	// next server start, so toggling it needs a reload.
	const profilingEnabled = workspace.getConfiguration('cwtools').get<boolean>('profiling') ?? false;
	const serverEnv = profilingEnabled ? { ...process.env, CWTOOLS_PROFILE: '1' } : process.env;
	const serverOptions: ServerOptions = {
		run: { command: cfg.serverExe, transport: TransportKind.stdio, options: { env: serverEnv } },
		debug : { command: cfg.serverExe, transport: TransportKind.stdio, options: { env: serverEnv } }
	}

	const fileEvents = [
		workspace.createFileSystemWatcher("**/{events,common,history,map,map_data,prescripted_countries,flags,decisions,missions}/**/*.txt"),
		workspace.createFileSystemWatcher("**/{interface,gfx}/**/*.gui"),
		workspace.createFileSystemWatcher("**/{interface,gfx}/**/*.gfx"),
		workspace.createFileSystemWatcher("**/{interface}/**/*.sfx"),
		workspace.createFileSystemWatcher("**/{interface,gfx,fonts,music,sound}/**/*.asset"),
		workspace.createFileSystemWatcher("**/{localisation,localisation_synced,localization}/**/*.yml"),
		// .cwt rule files: the server lints them and builds its ruleset from
		// them, so an edit made outside the editor (git checkout, another tool)
		// is otherwise invisible until the user runs reloadrulesconfig.
		workspace.createFileSystemWatcher("**/*.cwt")
	]
	context.subscriptions.push(...fileEvents);

	// Forward the user's ignore globs + suppressed diagnostic codes to the server
	// so it skips those files and drops those codes when validating.
	const ignoreOptions = readIgnoreOptions();

	const diagnosticsCache = new DiagnosticsSignatureCache();

	const clientOptions: LanguageClientOptions = {
		documentSelector: [
			{ scheme: 'file', language: 'paradox' },
			// .cwt rule-config files: the server lints them structurally
			// (undefined type/enum/single_alias refs + parse errors) rather
			// than running the game-script validator. See cwtools-vscode#43.
			{ scheme: 'file', language: 'cwt' },
			// Localisation .yml files: under a localisation* folder they open as
			// our dedicated 'paradox-localisation' language (Paradox loc is not
			// real YAML: strings run to the last quote, KEY:0 version suffixes,
			// embedded [cmd]/$ref$/§colour/£icon). The server routes loc by path,
			// not language id, so it attaches the same. The 'yaml'+pattern entry
			// stays as a fallback for any loc file VS Code still opens as YAML.
			{ scheme: 'file', language: 'paradox-localisation' },
			{ scheme: 'file', language: 'yaml', pattern: '**/{localisation,localisation_synced,localization}/**/*.yml' }
		],
		synchronize: {
			// The `cwtools.*` settings use different names than the server's init
			// options (e.g. errors.ignore vs ignoredErrorCodes), so the library's
			// raw-section push would never deliver the mapped keys. We push the
			// mapped payload ourselves on change instead (see below).
			fileEvents: fileEvents
		},
		initializationOptions: {
			language: cfg.language === 'eu5' ? 'paradox' : cfg.language,
			rulesCache: cfg.rulesCache,
			localisationLanguages: workspace.getConfiguration('cwtools').get('localisation.languages'),
			hoverShowAllLanguages: workspace.getConfiguration('cwtools').get('localisation.hoverShowAllLanguages') ?? false,
				hoverDebug: workspace.getConfiguration('cwtools').get('hover.debug') ?? false,
				hoverScopeDisplay: workspace.getConfiguration('cwtools').get('hover.scopeDisplay') ?? 'context',
			// Inlay hints. The server reads both at initialize only — neither key is
			// in its didChangeConfiguration handler — so a change needs a window
			// reload, which the setting descriptions say.
			inlayHintsLocTitles: workspace.getConfiguration('cwtools').get<boolean>('inlayHints.locTitles') ?? true,
			inlayHintsScopes: workspace.getConfiguration('cwtools').get<boolean>('inlayHints.scopes') ?? false,
			// Persistent cache dir + the user's vanilla install path. The Rust
			// server caches the base-game index here keyed by game version, so
			// it isn't re-parsed every startup. Passing the explicit install
			// path avoids relying on Steam auto-discovery.
			cacheDir: path.join(cfg.cacheDir, 'vanilla'),
			vanilla: workspace.getConfiguration('cwtools').get('cache.' + cfg.language),
			ignoreFilePatterns: ignoreOptions.ignoreFilePatterns,
			ignoredErrorCodes: ignoreOptions.ignoredErrorCodes,
			backgroundReindexIntervalMinutes: readBackgroundReindexMinutes(),
			backgroundReindexIdleSeconds: readBackgroundReindexIdleSeconds() },
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
		middleware: {
			handleDiagnostics: (uri, diagnostics, next) => {
				if (diagnosticsCache.shouldPublish(uri.toString(), diagnostics)) {
					next(uri, diagnostics);
				}
			},
			executeCommand: async (command, args, next) => {
				// genlocall returns generated loc stubs to open, not a toast string.
				if (command === 'genlocall') {
					try {
						const result = await next(command, args);
						await openGeneratedLoc(result);
						return result;
					} catch (err) {
						const msg = err instanceof Error ? err.message : String(err);
						window.showErrorMessage(`CWTools: genlocall failed: ${msg}`);
						return undefined;
					}
				}
				const isStatusCommand = command === 'cacheVanilla' || command === 'clearAllCaches' || command === 'reloadrulesconfig' || command === 'reindexWorkspace';
				if (!isStatusCommand) {
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

	// Client clears the DiagnosticCollection on stop; drop the cache too or the
	// re-publish after restart looks unchanged and squiggles don't return.
	context.subscriptions.push(client.onDidChangeState(e => {
		if (e.oldState === State.Running) {
			diagnosticsCache.clear();
		}
	}));

	// Push the mapped ignore/suppression settings and the background-reindex
	// interval to the server whenever they change, so a live edit to
	// `cwtools.errors.ignore`, the ignore globs, or the reindex interval takes
	// effect without a window reload. We drive this ourselves rather than via
	// synchronize.configurationSection, which would send the raw (unmapped)
	// `cwtools` section the server can't read.
	context.subscriptions.push(workspace.onDidChangeConfiguration(e => {
		const touched = e.affectsConfiguration('cwtools.errors.ignore')
			|| e.affectsConfiguration('cwtools.errors.ignorefiles')
			|| e.affectsConfiguration('cwtools.ignore_patterns')
			|| e.affectsConfiguration('cwtools.backgroundReindex.intervalMinutes')
			|| e.affectsConfiguration('cwtools.backgroundReindex.idleSeconds');
		if (!touched) { return; }
		const settings = buildReindexSettingsPayload(readIgnoreOptions(), readBackgroundReindexMinutes(), readBackgroundReindexIdleSeconds());
		client.sendNotification(DidChangeConfigurationNotification.type, { settings })
			.catch(err => logError('Failed to push updated settings to the server', err));
	}));

	return client;
}
