import * as assert from "assert";
import { minimatch } from "minimatch";
import { beforeEach, suite, test, vi } from "vitest";
import { LSPErrorCodes } from "vscode-languageserver-protocol";
import type { ExtensionContext } from "vscode";
import type { LanguageClientOptions } from "vscode-languageclient/node";

const {
	createdWatchers,
	createFileSystemWatcher,
	disposable,
	lastClientOptions,
	lastClient,
	configurationValues,
	requestType,
	progressToken,
	withProgress,
	showInformationMessage,
	showErrorMessage,
	openTextDocument,
	showTextDocument,
} = vi.hoisted(() => {
	const createdWatchers: { glob: string; dispose: () => void }[] = [];
	const configurationValues = new Map<string, unknown>();
	const lastClient: { value: unknown } = { value: undefined };
	const progressToken = {
		isCancellationRequested: false,
		onCancellationRequested: () => ({ dispose: () => undefined }),
	};
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
		lastClient,
		configurationValues,
		requestType: {},
		progressToken,
		withProgress: vi.fn(
			(
				_options: unknown,
				task: (
					progress: { report: (value: unknown) => void },
					token: unknown,
				) => Promise<unknown>,
			): Promise<unknown> => task({ report: () => undefined }, progressToken),
		),
		showInformationMessage: vi.fn(),
		showErrorMessage: vi.fn(),
		openTextDocument: vi.fn(),
		showTextDocument: vi.fn(),
	};
});

vi.mock("vscode", async (importOriginal) => ({
	...(await importOriginal<object>()),
	CancellationError: class extends Error {},
	ProgressLocation: { Notification: 15 },
	Uri: {
		parse: (value: string) => ({
			fsPath: decodeURIComponent(value.replace(/^file:\/\//, "")),
		}),
	},
	window: {
		createOutputChannel: () => ({ appendLine: () => {} }),
		withProgress,
		showInformationMessage,
		showErrorMessage,
		showTextDocument,
	},
	workspace: {
		createFileSystemWatcher,
		getConfiguration: () => ({
			get: (key: string) => configurationValues.get(key),
		}),
		onDidChangeConfiguration: vi.fn(() => disposable),
		openTextDocument,
	},
}));

vi.mock("vscode-languageclient/node", () => ({
	DidChangeConfigurationNotification: { type: {} },
	ExecuteCommandRequest: { type: requestType },
	ErrorAction: { Continue: 1, Shutdown: 2 },
	CloseAction: { DoNotRestart: 1, Restart: 2 },
	LanguageClient: class {
		constructor(
			_id: string,
			_name: string,
			_server: unknown,
			options: LanguageClientOptions,
		) {
			lastClientOptions.value = options;
			lastClient.value = this;
		}

		onDidChangeState(): { dispose: () => void } {
			return disposable;
		}
	},
	RevealOutputChannelOn: { Never: 4 },
	State: { Running: 2 },
	TransportKind: { stdio: 0 },
}));

import { createLanguageClient } from "../../src/host/lspClient";

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

function create(onStopped: () => void = () => {}): {
	context: ExtensionContext;
} {
	const context = { subscriptions: [] } as unknown as ExtensionContext;
	createLanguageClient(
		context,
		{
			language: "hoi4",
			serverExe: "/bin/cwtools-server",
			cacheDir: "/cache",
			rulesCache: "/rules",
		},
		onStopped,
	);
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
		configurationValues.clear();
	});

	test("re-reads initialization settings for each client start", () => {
		configurationValues.set("localisation.languages", ["English"]);
		create();
		const initializationOptions: unknown =
			lastClientOptions.value?.initializationOptions;
		assert.strictEqual(typeof initializationOptions, "function");
		const first = (
			initializationOptions as () => {
				localisationLanguages: string[];
			}
		)();
		configurationValues.set("localisation.languages", ["French"]);
		const second = (
			initializationOptions as () => {
				localisationLanguages: string[];
			}
		)();
		assert.deepStrictEqual(first.localisationLanguages, ["English"]);
		assert.deepStrictEqual(second.localisationLanguages, ["French"]);
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
		const middleware =
			lastClientOptions.value?.middleware?.workspace?.didChangeWatchedFile;
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

suite("lspClient — restart-limiting error handler", () => {
	beforeEach(() => {
		lastClientOptions.value = undefined;
		configurationValues.clear();
	});

	test("restarts up to the limit, then stops and calls onStopped", async () => {
		const stopped: number[] = [];
		create(() => stopped.push(Date.now()));
		const errorHandler = lastClientOptions.value?.errorHandler;
		assert.ok(errorHandler, "no errorHandler set on clientOptions");
		// 4 restarts allowed, the 5th within the 3-minute window gives up.
		for (let i = 0; i < 4; i++) {
			const result = await errorHandler.closed();
			assert.strictEqual(result.action, 2 /* CloseAction.Restart */);
		}
		assert.strictEqual(stopped.length, 0, "onStopped fired too early");
		const result = await errorHandler.closed();
		assert.strictEqual(result.action, 1 /* CloseAction.DoNotRestart */);
		assert.strictEqual(stopped.length, 1, "onStopped should fire once");
	});

	test("crashes spread past the 3-minute window keep restarting", async () => {
		vi.useFakeTimers();
		try {
			vi.setSystemTime(0);
			const stopped: number[] = [];
			create(() => stopped.push(1));
			const errorHandler = lastClientOptions.value?.errorHandler;
			assert.ok(errorHandler, "no errorHandler set on clientOptions");
			for (let i = 0; i < 4; i++) {
				const result = await errorHandler.closed();
				assert.strictEqual(result.action, 2 /* CloseAction.Restart */);
			}
			// The 5th crash lands after the first has left the window, so the
			// oldest is shifted out and the client restarts again.
			vi.setSystemTime(3 * 60 * 1000 + 1);
			const result = await errorHandler.closed();
			assert.strictEqual(result.action, 2 /* CloseAction.Restart */);
			assert.strictEqual(stopped.length, 0, "onStopped must not fire");
		} finally {
			vi.useRealTimers();
		}
	});

	test("transport errors continue three times, then shut down", async () => {
		create();
		const errorHandler = lastClientOptions.value?.errorHandler;
		assert.ok(errorHandler, "no errorHandler set on clientOptions");
		const boom = new Error("boom");
		for (const count of [1, 2, 3]) {
			const result = await errorHandler.error(boom, undefined, count);
			assert.strictEqual(
				result.action,
				1 /* ErrorAction.Continue */,
				`count ${count}`,
			);
		}
		for (const count of [0, 4, undefined]) {
			const result = await errorHandler.error(boom, undefined, count);
			assert.strictEqual(
				result.action,
				2 /* ErrorAction.Shutdown */,
				`count ${count}`,
			);
		}
	});
});

suite("lspClient — executeCommand middleware", () => {
	interface FakeClient {
		initializeResult?: {
			capabilities: {
				executeCommandProvider?: {
					commands: string[];
					workDoneProgress?: boolean;
				};
			};
		};
		sendRequest: ReturnType<typeof vi.fn>;
	}

	type Middleware = NonNullable<
		NonNullable<LanguageClientOptions["middleware"]>["executeCommand"]
	>;

	function serverCommands(
		commands: string[],
		workDoneProgress?: boolean,
	): FakeClient["initializeResult"] {
		return {
			capabilities: {
				executeCommandProvider: {
					commands,
					...(workDoneProgress ? { workDoneProgress: true } : {}),
				},
			},
		};
	}

	// createLanguageClient hands its real LanguageClient instance to the
	// middleware closure, so the fake client's sendRequest has to be set on
	// that instance for the middleware's requests to reach the stub.
	function middlewareSetup(): { middleware: Middleware; client: FakeClient } {
		create();
		const middleware = lastClientOptions.value?.middleware?.executeCommand;
		assert.ok(middleware, "no executeCommand middleware");
		const client = lastClient.value as FakeClient;
		client.sendRequest = vi.fn().mockResolvedValue("");
		return { middleware, client };
	}

	beforeEach(() => {
		vi.clearAllMocks();
		lastClientOptions.value = undefined;
		configurationValues.clear();
		progressToken.isCancellationRequested = false;
	});

	test("getGraphData sends the exact request with no workDoneToken and passes the graph through", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["getGraphData"], true);
		const graphData = [{ id: "a" }];
		client.sendRequest.mockResolvedValue(graphData);
		const next = vi.fn();

		const result: unknown = await middleware("getGraphData", ["idea", 3], next);

		// The panel renders whatever it gets back, so the result must be the
		// server's value, not a toast or an undefined.
		assert.strictEqual(result, graphData);
		assert.deepStrictEqual(client.sendRequest.mock.calls, [
			[
				requestType,
				{ command: "getGraphData", arguments: ["idea", 3] },
				progressToken,
			],
		]);
		// The server has no graceful cancel for this command, so the request
		// must not carry a token that would advertise one.
		assert.strictEqual(
			(client.sendRequest.mock.calls[0]?.[1] as { workDoneToken?: string })
				.workDoneToken,
			undefined,
		);
		assert.deepStrictEqual(withProgress.mock.calls[0]?.[0], {
			location: 15,
			title: "CWTools: Build graph",
			cancellable: true,
		});
		assert.deepStrictEqual(next.mock.calls, []);
	});

	test("getGraphData failures reach the caller instead of becoming a toast", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["getGraphData"]);
		const failure = new Error("server down");
		client.sendRequest.mockRejectedValue(failure);
		const next = vi.fn();

		await assert.rejects(
			async () => {
				await middleware("getGraphData", ["idea", 3], next);
			},
			(err: unknown) => err === failure,
		);
		assert.deepStrictEqual(showErrorMessage.mock.calls, []);
		assert.deepStrictEqual(showInformationMessage.mock.calls, []);
		assert.deepStrictEqual(next.mock.calls, []);
	});

	test("genlocall opens each non-empty stub as an untitled document with a BOM", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["genlocall"]);
		const stubs = [
			{ content: 'KEY:0 "text"' },
			{ content: "" },
			"not-a-stub",
			{ content: 'OTHER:0 "more"' },
		];
		client.sendRequest.mockResolvedValue(stubs);
		openTextDocument.mockImplementation((options: { content: string }) =>
			Promise.resolve({ content: options.content }),
		);
		const next = vi.fn();

		const result: unknown = await middleware("genlocall", [], next);

		assert.strictEqual(result, stubs);
		// Paradox loc files need the UTF-8 BOM; a manual save keeps it.
		assert.deepStrictEqual(openTextDocument.mock.calls, [
			[
				{
					content: '\uFEFFKEY:0 "text"',
					language: "paradox-localisation",
				},
			],
			[
				{
					content: '\uFEFFOTHER:0 "more"',
					language: "paradox-localisation",
				},
			],
		]);
		assert.deepStrictEqual(showTextDocument.mock.calls, [
			[{ content: '\uFEFFKEY:0 "text"' }, { preview: false }],
			[{ content: '\uFEFFOTHER:0 "more"' }, { preview: false }],
		]);
		assert.deepStrictEqual(next.mock.calls, []);
	});

	test("genlocall with no stubs reports that nothing was missing", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["genlocall"]);
		client.sendRequest.mockResolvedValue([]);
		const next = vi.fn();

		const result: unknown = await middleware("genlocall", [], next);

		assert.deepStrictEqual(showInformationMessage.mock.calls, [
			["CWTools: no missing localisation found."],
		]);
		assert.deepStrictEqual(openTextDocument.mock.calls, []);
		// The server's value still comes back; only the error/cancel paths
		// collapse to undefined.
		assert.deepStrictEqual(result, []);
	});

	test("genlocall cancellation is reported and yields no result", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["genlocall"]);
		client.sendRequest.mockRejectedValue({
			code: LSPErrorCodes.RequestCancelled,
		});
		const next = vi.fn();

		const result: unknown = await middleware("genlocall", [], next);

		assert.deepStrictEqual(showInformationMessage.mock.calls, [
			["CWTools: genlocall cancelled."],
		]);
		assert.deepStrictEqual(showErrorMessage.mock.calls, []);
		assert.strictEqual(result, undefined);
	});

	test("genlocall failures show an error and yield no result", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["genlocall"]);
		client.sendRequest.mockRejectedValue(new Error("boom"));
		const next = vi.fn();

		const result: unknown = await middleware("genlocall", [], next);

		assert.deepStrictEqual(showErrorMessage.mock.calls, [
			["CWTools: genlocall failed: boom"],
		]);
		assert.deepStrictEqual(showInformationMessage.mock.calls, []);
		assert.strictEqual(result, undefined);
	});

	test("reindexWorkspace goes through the progress notification and shows the server's reply", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["reindexWorkspace"]);
		client.sendRequest.mockResolvedValue("Workspace re-indexed.");
		const next = vi.fn();

		const result: unknown = await middleware("reindexWorkspace", [], next);

		assert.strictEqual(result, "Workspace re-indexed.");
		assert.deepStrictEqual(withProgress.mock.calls[0]?.[0], {
			location: 15,
			title: "CWTools: Re-index workspace",
			cancellable: true,
		});
		assert.deepStrictEqual(client.sendRequest.mock.calls, [
			[
				requestType,
				{ command: "reindexWorkspace", arguments: [] },
				progressToken,
			],
		]);
		assert.deepStrictEqual(showInformationMessage.mock.calls, [
			["CWTools: Workspace re-indexed."],
		]);
		assert.deepStrictEqual(next.mock.calls, []);
	});

	test("each known server command gets its progress title", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands([
			"cacheVanilla",
			"clearAllCaches",
			"reloadrulesconfig",
		]);
		const next = vi.fn();

		for (const command of [
			"cacheVanilla",
			"clearAllCaches",
			"reloadrulesconfig",
		]) {
			await middleware(command, [], next);
		}

		assert.deepStrictEqual(
			withProgress.mock.calls.map(
				(call) => (call[0] as { title: string }).title,
			),
			[
				"CWTools: Regenerate game vanilla cache file",
				"CWTools: Clear all caches and reindex",
				"CWTools: Reload config rules",
			],
		);
	});

	test("known commands without a string result show no toast", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["reindexWorkspace"]);
		client.sendRequest.mockResolvedValue(undefined);
		const next = vi.fn();

		const result: unknown = await middleware("reindexWorkspace", [], next);

		assert.deepStrictEqual(showInformationMessage.mock.calls, []);
		assert.strictEqual(result, undefined);
	});

	test("known commands report cancellation and yield no result", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["reindexWorkspace"]);
		client.sendRequest.mockRejectedValue({
			code: LSPErrorCodes.ServerCancelled,
		});
		const next = vi.fn();

		const result: unknown = await middleware("reindexWorkspace", [], next);

		assert.deepStrictEqual(showInformationMessage.mock.calls, [
			["CWTools: reindexWorkspace cancelled."],
		]);
		assert.deepStrictEqual(showErrorMessage.mock.calls, []);
		assert.strictEqual(result, undefined);
	});

	test("known command failures show an error and yield no result", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["reindexWorkspace"]);
		client.sendRequest.mockRejectedValue(new Error("boom"));
		const next = vi.fn();

		const result: unknown = await middleware("reindexWorkspace", [], next);

		assert.deepStrictEqual(showErrorMessage.mock.calls, [
			["CWTools: reindexWorkspace failed: boom"],
		]);
		assert.deepStrictEqual(showInformationMessage.mock.calls, []);
		assert.strictEqual(result, undefined);
	});

	test("unknown commands are delegated untouched", async () => {
		const { middleware, client } = middlewareSetup();
		const next = vi.fn().mockResolvedValue("server says");

		const result: unknown = await middleware(
			"someOtherCommand",
			[1, "a"],
			next,
		);

		assert.strictEqual(result, "server says");
		assert.deepStrictEqual(next.mock.calls, [["someOtherCommand", [1, "a"]]]);
		assert.deepStrictEqual(withProgress.mock.calls, []);
		assert.deepStrictEqual(showInformationMessage.mock.calls, []);
		assert.deepStrictEqual(showErrorMessage.mock.calls, []);
		assert.deepStrictEqual(client.sendRequest.mock.calls, []);
	});

	test("server-advertised commands without a progress title are delegated too", async () => {
		const { middleware, client } = middlewareSetup();
		client.initializeResult = serverCommands(["getFileTypes"]);
		const next = vi.fn().mockResolvedValue(["event"]);

		const result: unknown = await middleware("getFileTypes", ["x"], next);

		assert.deepStrictEqual(result, ["event"]);
		assert.deepStrictEqual(next.mock.calls, [["getFileTypes", ["x"]]]);
		assert.deepStrictEqual(withProgress.mock.calls, []);
	});
});
