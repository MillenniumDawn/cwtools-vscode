import * as path from 'path';
import type { ExtensionContext } from 'vscode';
import { workspace, window } from 'vscode';
import type { LanguageClientOptions, ServerOptions } from 'vscode-languageclient/node';
import { LanguageClient, TransportKind, RevealOutputChannelOn } from 'vscode-languageclient/node';
import { GAME_IDS } from './games';

export interface ClientConfig {
	language: string;
	isVanillaFolder: boolean;
	serverExe: string;
	cacheDir: string;
	rulesCache: string;
	repoPath?: string;
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

	const clientOptions: LanguageClientOptions = {
		documentSelector: [
			{ scheme: 'file', language: 'paradox' },
			...GAME_IDS.map(id => ({ scheme: 'file', language: id })),
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
			configurationSection: 'cwtools',
			fileEvents: fileEvents
		},
		initializationOptions: {
			language: cfg.language === 'eu5' ? 'paradox' : cfg.language,
			isVanillaFolder: cfg.isVanillaFolder,
			rulesCache: cfg.rulesCache,
			rules_version: workspace.getConfiguration('cwtools').get('rules_version'),
			repoPath: cfg.repoPath,
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

	return new LanguageClient('cwtools', 'Paradox Language Server', serverOptions, clientOptions);
}
