import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";
import type { ExtensionContext } from "vscode";
import type { LanguageClientOptions } from "vscode-languageclient/node";

const {
	createdWatchers,
	createFileSystemWatcher,
	disposable,
	lastClientOptions,
} = vi.hoisted(() => {
	const createdWatchers: { glob: string; dispose: () => void }[] = [];
	return {
		createdWatchers,
		createFileSystemWatcher: vi.fn((glob: string) => {
			const watcher = { glob, dispose: () => {} };
			createdWatchers.push(watcher);
			return watcher;
		}),
		disposable: { dispose: () => {} },
		lastClientOptions: {
			value: undefined as LanguageClientOptions | undefined,
		},
	};
});

vi.mock("vscode", () => ({
	CancellationError: class extends Error {},
	ProgressLocation: { Notification: 15 },
	window: {
		createOutputChannel: () => ({ appendLine: () => {} }),
		showErrorMessage: vi.fn(),
		showInformationMessage: vi.fn(),
	},
	workspace: {
		createFileSystemWatcher,
		getConfiguration: () => ({ get: () => undefined }),
		onDidChangeConfiguration: vi.fn(() => disposable),
	},
}));

vi.mock("vscode-languageclient/node", () => ({
	DidChangeConfigurationNotification: { type: {} },
	ExecuteCommandRequest: { type: {} },
	LanguageClient: class {
		constructor(
			_id: string,
			_name: string,
			_server: unknown,
			options: LanguageClientOptions,
		) {
			lastClientOptions.value = options;
		}

		onDidChangeState(): { dispose: () => void } {
			return disposable;
		}
	},
	RevealOutputChannelOn: { Never: 4 },
	State: { Running: 2 },
	TransportKind: { stdio: 0 },
}));

import { createLanguageClient } from "../../extension/lspClient";

// The server's workspace scan walks the whole tree and filters by
// cwtools_file_manager's SCRIPT_EXTENSIONS (txt, gui, gfx, sfx, asset, map),
// and reads yml/yaml/csv under a localisation dir. A watcher narrower than
// that leaves edits made outside the editor unindexed (#117).
const EXPECTED_GLOBS = [
	"**/*.{txt,gui,gfx,sfx,asset,map}",
	"**/{localisation,localisation_synced,localization}/**/*.{yml,yaml,csv}",
	"**/*.cwt",
];

function create(): { context: ExtensionContext } {
	const context = { subscriptions: [] } as unknown as ExtensionContext;
	createLanguageClient(context, {
		language: "hoi4",
		serverExe: "/bin/cwtools-server",
		cacheDir: "/cache",
		rulesCache: "/rules",
	});
	return { context };
}

suite("lspClient — watched files", () => {
	beforeEach(() => {
		createdWatchers.length = 0;
		createFileSystemWatcher.mockClear();
		lastClientOptions.value = undefined;
	});

	test("watches every file class the server indexes", () => {
		create();
		assert.deepStrictEqual(
			createdWatchers.map((w) => w.glob),
			EXPECTED_GLOBS,
		);
	});

	test("hands the watchers to the client and disposes them with the extension", () => {
		const { context } = create();
		assert.deepStrictEqual(
			lastClientOptions.value?.synchronize?.fileEvents,
			createdWatchers,
		);
		for (const watcher of createdWatchers) {
			assert.ok(
				context.subscriptions.includes(watcher),
				`watcher ${watcher.glob} not registered for disposal`,
			);
		}
	});
});
