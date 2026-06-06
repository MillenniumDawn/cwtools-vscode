/**
 * Drives a single cwtools server engine over stdio for the parity harness.
 *
 * Both engines speak LSP, but they are initialised differently:
 *   - The Rust server loads rules straight from initializationOptions.rulesCache.
 *   - The F# server loads rules lazily, only once it receives a
 *     workspace/didChangeConfiguration carrying the full `cwtools` settings
 *     block (and it throws on any missing key, so every key must be present).
 *
 * The extension itself papers over this difference; here we reproduce just
 * enough of its handshake to ask both engines the same questions.
 */
import { readFileSync } from 'fs';
import * as path from 'path';
import { pathToFileURL } from 'url';
import { LspClient } from './lspClient';
import { extractCompletionLabel } from '../support/utils';

export type Engine = 'rust' | 'fsharp';

export interface SessionOptions {
	serverExe: string;
	engine: Engine;
	language: string;       // e.g. 'stellaris'
	rulesLeaf: string;      // folder containing the .cwt rule files
	workspaceRoot: string;  // the sample mod root
}

const sleep = (ms: number) => new Promise<void>(r => setTimeout(r, ms));

// A complete `cwtools` settings block. The F# server reads every one of these
// keys with a JSON accessor that throws when a key is absent, so we send them
// all even though most stay at their defaults.
function cwtoolsSettings(rulesLeaf: string) {
	return {
		localisation: { languages: [], generated_strings: '' },
		errors: { vanilla: false, ignore: [], ignorefiles: [] },
		experimental: false,
		debug_mode: false,
		ignore_patterns: [],
		trace: { server: 'off' },
		cache: { eu4: '', stellaris: '', hoi4: '', ck2: '', imperator: '', vic2: '', ck3: '', vic3: '', eu5: '' },
		rules_folder: rulesLeaf,
		rules_version: 'manual',
		maxFileSize: 10,
	};
}

export class EngineSession {
	private readonly client: LspClient;
	private loaded = false;
	private readonly diagnostics = new Map<string, unknown[]>();

	private constructor(private readonly opts: SessionOptions) {
		this.client = new LspClient(opts.serverExe);
		this.client.onNotification((method, params) => {
			if (method === 'loadingBar') {
				const p = params as { enable?: boolean };
				if (p && p.enable === false) this.loaded = true;
			}
			if (method === 'textDocument/publishDiagnostics') {
				const p = params as { uri: string; diagnostics: unknown[] };
				if (p?.uri) this.diagnostics.set(p.uri, p.diagnostics);
			}
		});
	}

	get engine(): Engine { return this.opts.engine; }

	static async start(opts: SessionOptions): Promise<EngineSession> {
		const s = new EngineSession(opts);
		await s.handshake();
		return s;
	}

	private uri(file: string): string {
		return pathToFileURL(file).href;
	}

	private async handshake(): Promise<void> {
		const { workspaceRoot, language, rulesLeaf } = this.opts;
		await this.client.request('initialize', {
			processId: process.pid,
			rootUri: this.uri(workspaceRoot),
			rootPath: workspaceRoot,
			capabilities: {
				textDocument: {
					hover: { contentFormat: ['markdown', 'plaintext'] },
					completion: { completionItem: { snippetSupport: true } },
					definition: {},
					references: {},
					formatting: { dynamicRegistration: false },
				},
			},
			initializationOptions: {
				language,
				isVanillaFolder: false,
				// Rust reads rules from here; F# (manual mode) reads rules_folder
				// from the settings block instead, so the exact value is only
				// load-bearing for Rust.
				rulesCache: this.opts.engine === 'rust' ? rulesLeaf : path.dirname(rulesLeaf),
				rules_version: 'manual',
				repoPath: 'https://github.com/cwtools/cwtools-stellaris-config',
				diagnosticLogging: false,
			},
		});
		this.client.notify('initialized', {});
		this.client.notify('workspace/didChangeConfiguration', {
			settings: { cwtools: cwtoolsSettings(rulesLeaf) },
		});
	}

	/** Open a document so the server will answer requests against it. */
	openDocument(file: string): void {
		const text = readFileSync(file, 'utf8');
		this.client.notify('textDocument/didOpen', {
			textDocument: { uri: this.uri(file), languageId: this.opts.language, version: 1, text },
		});
		// The extension tells the server which file is focused; some analysis
		// is gated on this.
		this.client.notify('didFocusFile', { uri: this.uri(file) });
	}

	/**
	 * Wait until the server has finished its first pass. Falls back to a fixed
	 * delay if the loading-bar signal never arrives.
	 */
	async waitUntilLoaded(timeoutMs = 45000): Promise<void> {
		const deadline = Date.now() + timeoutMs;
		while (!this.loaded && Date.now() < deadline) {
			await sleep(500);
		}
		// Give the server a beat to settle even after it reports done.
		await sleep(500);
	}

	async hover(file: string, line: number, character: number): Promise<string> {
		const res = await this.client.request<{ contents?: { value?: string } | { value?: string }[] }>(
			'textDocument/hover',
			{ textDocument: { uri: this.uri(file) }, position: { line, character } }
		);
		const contents = res?.contents;
		if (!contents) return '';
		if (Array.isArray(contents)) return contents.map(c => c.value ?? '').join('\n');
		return contents.value ?? '';
	}

	async completionLabels(file: string, line: number, character: number): Promise<string[]> {
		const res = await this.client.request<
			{ items?: { label?: string | { label?: string } }[] } | { label?: string | { label?: string } }[]
		>('textDocument/completion', { textDocument: { uri: this.uri(file) }, position: { line, character } });
		const items = Array.isArray(res) ? res : (res?.items ?? []);
		return items.map(i => extractCompletionLabel(i)).filter(Boolean);
	}

	async definition(file: string, line: number, character: number): Promise<{ uri: string; range: { start: { line: number; character: number } } }[]> {
		const res = await this.client.request<
			| { uri: string; range: { start: { line: number; character: number } } }
			| { uri: string; range: { start: { line: number; character: number } } }[]
			| null
		>('textDocument/definition', { textDocument: { uri: this.uri(file) }, position: { line, character } });
		if (!res) return [];
		return Array.isArray(res) ? res : [res];
	}

	async references(file: string, line: number, character: number): Promise<{ uri: string; range: { start: { line: number; character: number } } }[]> {
		const res = await this.client.request<
			{ uri: string; range: { start: { line: number; character: number } } }[] | null
		>('textDocument/references', {
			textDocument: { uri: this.uri(file) },
			position: { line, character },
			context: { includeDeclaration: true },
		});
		return res ?? [];
	}

	getDiagnostics(file: string): unknown[] {
		return this.diagnostics.get(this.uri(file)) ?? [];
	}

	async formatting(file: string): Promise<{ range: { start: { line: number; character: number }; end: { line: number; character: number } }; newText: string }[]> {
		const res = await this.client.request<
			{ range: { start: { line: number; character: number }; end: { line: number; character: number } }; newText: string }[] | null
		>('textDocument/documentFormatting', {
			textDocument: { uri: this.uri(file) },
			options: { tabSize: 4, insertSpaces: true },
		});
		return res ?? [];
	}

	dispose(): void {
		this.client.dispose();
	}
}
