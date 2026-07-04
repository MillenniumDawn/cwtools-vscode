import * as path from 'path';
import type { ExtensionContext } from 'vscode';
import { workspace, window } from 'vscode';
import type { LanguageClientOptions, ServerOptions } from 'vscode-languageclient/node';
import { LanguageClient, TransportKind, RevealOutputChannelOn, DidChangeConfigurationNotification } from 'vscode-languageclient/node';

export interface ClientConfig {
	language: string;
	isVanillaFolder: boolean;
	serverExe: string;
	cacheDir: string;
	rulesCache: string;
	repoPath?: string;
}

// The server reads remapped keys (ignoreFilePatterns/ignoredErrorCodes); the
// `cwtools.*` settings use different names. Map them here so both the initial
// initializationOptions and the live didChangeConfiguration payload agree.
// ignore_patterns are already globs; errors.ignorefiles lists bare file names,
// so turn each into a **/<name> glob to match anywhere.
function readIgnoreOptions(): { ignoreFilePatterns: string[]; ignoredErrorCodes: string[] } {
	const cfg = workspace.getConfiguration('cwtools');
	const ignorePatterns = cfg.get<string[]>('ignore_patterns') ?? [];
	const ignoreFiles = cfg.get<string[]>('errors.ignorefiles') ?? [];
	return {
		ignoreFilePatterns: [
			...ignorePatterns,
			...ignoreFiles.map(f => (f.includes('/') ? f : `**/${f}`)),
		],
		ignoredErrorCodes: cfg.get<string[]>('errors.ignore') ?? [],
	};
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
		workspace.createFileSystemWatcher("**/{events,common,map,map_data,prescripted_countries,flags,decisions,missions}/**/*.txt"),
		workspace.createFileSystemWatcher("**/{interface,gfx}/**/*.gui"),
		workspace.createFileSystemWatcher("**/{interface,gfx}/**/*.gfx"),
		workspace.createFileSystemWatcher("**/{interface}/**/*.sfx"),
		workspace.createFileSystemWatcher("**/{interface,gfx,fonts,music,sound}/**/*.asset"),
		workspace.createFileSystemWatcher("**/{localisation,localisation_synced,localization}/**/*.yml"),
		// Watch cached CWT rule files; force posix separators so the glob works on Windows.
		workspace.createFileSystemWatcher(cfg.cacheDir.replace(/\\/g, '/') + '/**/*.cwt')
	]
	context.subscriptions.push(...fileEvents);

	// Forward the user's ignore globs + suppressed diagnostic codes to the server
	// so it skips those files and drops those codes when validating.
	const ignoreOptions = readIgnoreOptions();

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
			// Persistent cache dir + the user's vanilla install path. The Rust
			// server caches the base-game index here keyed by game version, so
			// it isn't re-parsed every startup. Passing the explicit install
			// path avoids relying on Steam auto-discovery.
			cacheDir: path.join(cfg.cacheDir, 'vanilla'),
			vanilla: workspace.getConfiguration('cwtools').get('cache.' + cfg.language),
			ignoreFilePatterns: ignoreOptions.ignoreFilePatterns,
			ignoredErrorCodes: ignoreOptions.ignoredErrorCodes },
			revealOutputChannelOn: RevealOutputChannelOn.Error,
		// The server advertises its commands (cacheVanilla, clearAllCaches,
		// reloadrulesconfig, genlocall, ...) in executeCommandProvider, and
		// vscode-languageclient registers each as a VS Code command. Registering
		// them ourselves too makes client.start() throw "command already exists",
		// so the UX (result toasts, opening the generated loc) lives here instead.
		middleware: {
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
				const isStatusCommand = command === 'cacheVanilla' || command === 'clearAllCaches' || command === 'reloadrulesconfig';
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

	// Push the mapped ignore/suppression settings to the server whenever they
	// change, so a live edit to `cwtools.errors.ignore` or the ignore globs takes
	// effect without a window reload. We drive this ourselves rather than via
	// synchronize.configurationSection, which would send the raw (unmapped)
	// `cwtools` section the server can't read.
	context.subscriptions.push(workspace.onDidChangeConfiguration(e => {
		const touched = e.affectsConfiguration('cwtools.errors.ignore')
			|| e.affectsConfiguration('cwtools.errors.ignorefiles')
			|| e.affectsConfiguration('cwtools.ignore_patterns');
		if (!touched) { return; }
		// Client not started yet; the initializationOptions already carry the
		// current values, so there is nothing to push.
		client.sendNotification(DidChangeConfigurationNotification.type, { settings: readIgnoreOptions() }).catch(() => {});
	}));

	return client;
}
