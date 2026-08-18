import * as assert from "assert";
import { minimatch } from "minimatch";
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
	Uri: {
		parse: (value: string) => ({
			fsPath: decodeURIComponent(value.replace(/^file:\/\//, "")),
		}),
	},
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
// and reads yml/yaml/csv under a localisation dir. Anything it indexes has to
// be watched, or an edit made outside the editor never reaches it (#117).
const WATCHED: [path: string, watched: boolean][] = [
	["portraits/leaders/x.txt", true],
	["dlc/dlc01/common/ideas/x.txt", true],
	["map_data/terrain.map", true],
	["interface/x.sfx", true],
	["gfx/models/x.asset", true],
	["localisation/english/a_l_english.yml", true],
	["localisation/replace/b.csv", true],
	["deep/nested/localization/c.yaml", true],
	["Config/events.cwt", true],
	// Loc extensions only count under a localisation dir, matching the
	// server's own loc predicate.
	["docs/notes.yml", false],
	["data/export.csv", false],
	// Resources the server notes but never reads.
	["gfx/flags/x.dds", false],
	["gfx/models/x.mesh", false],
	["music/track.ogg", false],
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

function watchedFileEvent(uri: string): { uri: string; type: 1 | 2 | 3 } {
	return { uri, type: 2 };
}

suite("lspClient — watched files", () => {
	beforeEach(() => {
		createdWatchers.length = 0;
		createFileSystemWatcher.mockClear();
		lastClientOptions.value = undefined;
	});

	test("the globs match every file class the server indexes", () => {
		create();
		const globs = createdWatchers.map((w) => w.glob);
		for (const [path, watched] of WATCHED) {
			assert.strictEqual(
				globs.some((glob) => minimatch(path, glob)),
				watched,
				path,
			);
		}
	});

	test("hands the watchers to the client and disposes them with the extension", () => {
		const { context } = create();
		const fileEvents = lastClientOptions.value?.synchronize?.fileEvents;
		assert.ok(Array.isArray(fileEvents), "fileEvents is not a watcher list");
		const byGlob = (a: { glob: string }, b: { glob: string }): number =>
			a.glob.localeCompare(b.glob);
		assert.deepStrictEqual(
			(fileEvents as unknown as { glob: string }[]).slice().sort(byGlob),
			createdWatchers.slice().sort(byGlob),
		);
		for (const watcher of createdWatchers) {
			assert.ok(
				context.subscriptions.includes(watcher),
				`watcher ${watcher.glob} not registered for disposal`,
			);
		}
	});

	test("holds back events for files the server's own walk skips", async () => {
		create();
		const forwarded: string[] = [];
		const next = (event: { uri: string }): Promise<void> => {
			forwarded.push(event.uri);
			return Promise.resolve();
		};
		const middleware = lastClientOptions.value?.middleware?.workspace
			?.didChangeWatchedFile;
		assert.ok(middleware, "no didChangeWatchedFile middleware");
		for (const uri of [
			"file:///mod/Changelog.txt",
			"file:///mod/dist/bundle.js.map",
			"file:///mod/common/ideas/x.txt",
			"file:///mod/My%20Mod/events/y.txt",
		]) {
			await middleware(watchedFileEvent(uri), next);
		}
		assert.deepStrictEqual(forwarded, [
			"file:///mod/common/ideas/x.txt",
			"file:///mod/My%20Mod/events/y.txt",
		]);
	});
});
