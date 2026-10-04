import * as assert from "assert";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { minimatch } from "minimatch";
import { beforeEach, suite, test, vi } from "vitest";
import type { Mock } from "vitest";
import { LSPErrorCodes } from "vscode-languageserver-protocol";
import type { ExtensionContext } from "vscode";
import type { LanguageClientOptions } from "vscode-languageclient/node";

const {
	createdWatchers,
	createFileSystemWatcher,
	disposable,
	stateChangeHandlers,
	lastClientOptions,
	lastClient,
	configurationValues,
	resolveRulesCache,
	requestType,
	progressToken,
	withProgress,
	showInformationMessage,
	showErrorMessage,
	openTextDocument,
	showTextDocument,
	executeCommand,
	onDidChangeConfiguration,
	sendNotification,
	sendRequest,
	logError,
} = vi.hoisted(() => {
	const createdWatchers: {
		glob: unknown;
		dispose: Mock<() => void>;
		fire: (
			event: "create" | "change" | "delete",
			uri: { toString: () => string },
		) => void;
	}[] = [];
	const stateChangeHandlers: ((event: {
		oldState: number;
		newState: number;
	}) => void)[] = [];
	const configurationValues = new Map<string, unknown>();
	const lastClient: { value: unknown } = { value: undefined };
	const progressToken = {
		isCancellationRequested: false,
		onCancellationRequested: () => ({ dispose: () => undefined }),
	};
	return {
		createdWatchers,
		createFileSystemWatcher: vi.fn((glob: unknown) => {
			const listeners = {
				create: [] as ((uri: { toString: () => string }) => void)[],
				change: [] as ((uri: { toString: () => string }) => void)[],
				delete: [] as ((uri: { toString: () => string }) => void)[],
			};
			const subscribe = (
				kind: "create" | "change" | "delete",
				listener: (uri: { toString: () => string }) => void,
			) => {
				let active = true;
				listeners[kind].push((uri) => {
					if (active) listener(uri);
				});
				return { dispose: () => (active = false) };
			};
			const watcher = {
				glob,
				dispose: vi.fn<() => void>(),
				onDidCreate: (listener: (uri: { toString: () => string }) => void) =>
					subscribe("create", listener),
				onDidChange: (listener: (uri: { toString: () => string }) => void) =>
					subscribe("change", listener),
				onDidDelete: (listener: (uri: { toString: () => string }) => void) =>
					subscribe("delete", listener),
				fire: (
					event: "create" | "change" | "delete",
					uri: { toString: () => string },
				) => {
					for (const listener of listeners[event]) listener(uri);
				},
			};
			createdWatchers.push(watcher);
			return watcher;
		}),
		disposable: { dispose: () => {} },
		lastClientOptions: {
			value: undefined as LanguageClientOptions | undefined,
		},
		lastClient,
		configurationValues,
		resolveRulesCache: vi.fn(() =>
			Promise.resolve({
				rulesCache: "/rules",
				fetchUpstream: false,
			}),
		),
		stateChangeHandlers,
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
		executeCommand: vi.fn(),
		onDidChangeConfiguration: vi.fn(() => disposable),
		sendNotification: vi
			.fn<(type: unknown, params: unknown) => Promise<void>>()
			.mockResolvedValue(undefined),
		sendRequest: vi
			.fn<(type: unknown, params: unknown) => Promise<unknown>>()
			.mockResolvedValue(undefined),
		logError: vi.fn(),
	};
});

vi.mock("vscode", async (importOriginal) => ({
	...(await importOriginal<object>()),
	CancellationError: class extends Error {},
	ProgressLocation: { Notification: 15 },
	Uri: {
		file: (fsPath: string) => ({ fsPath, toString: () => `file://${fsPath}` }),
		parse: (value: string) => ({
			fsPath: decodeURIComponent(value.replace(/^file:\/\//, "")),
			toString: () => value,
		}),
	},
	RelativePattern: class {
		constructor(
			readonly baseUri: { fsPath: string },
			readonly pattern: string,
		) {}
	},
	window: {
		createOutputChannel: () => ({ appendLine: () => {} }),
		withProgress,
		showInformationMessage,
		showErrorMessage,
		showTextDocument,
	},
	commands: { executeCommand },
	workspace: {
		createFileSystemWatcher,
		getWorkspaceFolder: (uri: { fsPath: string }) =>
			uri.fsPath.startsWith("/workspace/") || uri.fsPath === "/workspace"
				? { uri: { fsPath: "/workspace" } }
				: undefined,
		getConfiguration: () => ({
			get: (key: string) => configurationValues.get(key),
		}),
		onDidChangeConfiguration,
		openTextDocument,
	},
}));

vi.mock("../../src/host/logger", () => ({
	errorMessage: (err: unknown) =>
		err instanceof Error ? err.message : String(err),
	logError,
	outputChannel: { appendLine: () => undefined },
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

		sendNotification = sendNotification;
		sendRequest = sendRequest;

		onDidChangeState(
			handler: (event: { oldState: number; newState: number }) => void,
		): { dispose: () => void } {
			stateChangeHandlers.push(handler);
			return disposable;
		}
	},
	RevealOutputChannelOn: { Never: 4 },
	State: { Stopped: 0, Starting: 1, Running: 2 },
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

function create(
	onStopped: () => void = () => {},
	onRulesCacheChanged?: (rulesCache: string) => void,
	fetchRules?: () => void,
	{
		workspaceRoot = "/workspace",
		rulesCache = "/rules",
	}: { workspaceRoot?: string; rulesCache?: string } = {},
): {
	context: ExtensionContext;
} {
	const context = { subscriptions: [] } as unknown as ExtensionContext;
	createLanguageClient(
		context,
		{
			language: "hoi4",
			serverExe: "/bin/cwtools-server",
			cacheDir: "/cache",
			rulesCache,
			resolveRulesCache,
			onRulesCacheChanged,
			fetchRules,
			workspaceFolder: {
				uri: { fsPath: workspaceRoot },
				name: "workspace",
				index: 0,
			} as never,
		},
		onStopped,
	);
	return { context };
}

function watchedFileEvent(uri: string): { uri: string; type: 1 | 2 | 3 } {
	return { uri, type: 2 };
}

type ConfigurationChangeEvent = {
	affectsConfiguration(section: string): boolean;
};

function configurationChangeEvent(touched: string[]): ConfigurationChangeEvent {
	return {
		affectsConfiguration(section: string): boolean {
			return touched.includes(section);
		},
	};
}

function configurationChangeHandler(): (
	event: ConfigurationChangeEvent,
) => void {
	const calls = onDidChangeConfiguration.mock.calls as unknown as Array<
		[(event: ConfigurationChangeEvent) => void]
	>;
	const handler = calls[0]?.[0];
	assert.ok(handler, "no configuration change handler");
	return handler;
}

function rulesWatcherAt(
	root: string,
): (typeof createdWatchers)[number] | undefined {
	const matchingWatchers = createdWatchers.filter((watcher) => {
		if (typeof watcher.glob !== "object" || watcher.glob === null)
			return false;
		return (
			"baseUri" in watcher.glob &&
			(watcher.glob as { baseUri: { fsPath: string } }).baseUri.fsPath === root
		);
	});
	return matchingWatchers[matchingWatchers.length - 1];
}

function isRulesSettingsPayload(
	value: unknown,
): value is { settings: { rulesCache?: string } } {
	if (typeof value !== "object" || value === null || !("settings" in value))
		return false;
	const settings = value.settings;
	return (
		typeof settings === "object" &&
		settings !== null &&
		(!("rulesCache" in settings) ||
			settings.rulesCache === undefined ||
			typeof settings.rulesCache === "string")
	);
}

function lastSettingsPayload(): { settings: { rulesCache?: string } } {
	const calls = sendNotification.mock.calls;
	const call = calls[calls.length - 1];
	assert.ok(call, "no configuration notification was sent");
	const payload = call[1];
	assert.ok(isRulesSettingsPayload(payload), "invalid settings notification");
	return payload;
}

function fileUri(fsPath: string): {
	fsPath: string;
	toString: () => string;
} {
	return { fsPath, toString: () => `file://${fsPath}` };
}

suite("lspClient — watched files", () => {
	beforeEach(() => {
		createdWatchers.length = 0;
		createFileSystemWatcher.mockClear();
		lastClientOptions.value = undefined;
		configurationValues.clear();
		resolveRulesCache.mockReset();
		resolveRulesCache.mockResolvedValue({
			rulesCache: "/rules",
			fetchUpstream: false,
		});
		stateChangeHandlers.length = 0;
		onDidChangeConfiguration.mockClear();
		sendNotification.mockClear();
		sendRequest.mockClear();
	});

	test("re-reads initialization settings for each client start", () => {
		configurationValues.set("localisation.languages", ["English"]);
		configurationValues.set("formatting.maxLineWidth", 80);
		create();
		const initializationOptions: unknown =
			lastClientOptions.value?.initializationOptions;
		assert.strictEqual(typeof initializationOptions, "function");
		const first = (
			initializationOptions as () => {
				localisationLanguages: string[];
				formattingMaxLineWidth: number;
			}
		)();
		configurationValues.set("localisation.languages", ["French"]);
		const second = (
			initializationOptions as () => {
				localisationLanguages: string[];
				formattingMaxLineWidth: number;
			}
		)();
		assert.deepStrictEqual(first.localisationLanguages, ["English"]);
		assert.strictEqual(first.formattingMaxLineWidth, 80);
		assert.deepStrictEqual(second.localisationLanguages, ["French"]);
		assert.strictEqual(second.formattingMaxLineWidth, 80);
	});

	test("maps workspace-wide diagnostics in initialization and live settings", async () => {
		create();
		const initializationOptions = lastClientOptions.value
			?.initializationOptions as () => { workspaceWideDiagnostics: boolean };
		assert.strictEqual(initializationOptions().workspaceWideDiagnostics, true);

		configurationValues.set("diagnostics.workspaceWide", false);
		assert.strictEqual(initializationOptions().workspaceWideDiagnostics, false);

		configurationChangeHandler()(
			configurationChangeEvent(["cwtools.diagnostics.workspaceWide"]),
		);
		await Promise.resolve();

		const payload = sendNotification.mock.calls[0]?.[1] as {
			settings: { workspaceWideDiagnostics: boolean };
		};
		assert.strictEqual(payload.settings.workspaceWideDiagnostics, false);
	});

	test("locks the language client to the selected workspace folder", () => {
		create();
		const selected = lastClientOptions.value?.workspaceFolder as
			| { uri: { fsPath: string } }
			| undefined;
		assert.strictEqual(selected?.uri.fsPath, "/workspace");
	});

	test("the globs match every file class the server indexes", () => {
		create();
		const globs = createdWatchers
			.map((watcher) => watcher.glob)
			.filter((glob): glob is string => typeof glob === "string");
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
		const workspaceWatchers = createdWatchers.filter(
			(watcher): watcher is typeof watcher & { glob: string } =>
				typeof watcher.glob === "string",
		);
		assert.deepStrictEqual(
			(fileEvents as unknown as { glob: string }[]).slice().sort(byGlob),
			workspaceWatchers.slice().sort(byGlob),
		);
		for (const watcher of workspaceWatchers) {
			assert.ok(
				context.subscriptions.includes(watcher),
				`watcher ${watcher.glob} not registered for disposal`,
			);
		}
		assert.ok(
			context.subscriptions.some((subscription) => "dispose" in subscription),
			"rules watcher lifecycle must be registered for disposal",
		);
		const rulesPattern = rulesWatcherAt("/rules")?.glob as
			| { baseUri: { fsPath: string }; pattern: string }
			| undefined;
		assert.ok(rulesPattern, "rules watcher has no RelativePattern");
		assert.strictEqual(rulesPattern?.baseUri.fsPath, "/rules");
		assert.strictEqual(rulesPattern?.pattern, "**/*.cwt");
	});

	test("debounces a 50-file rules checkout into one reload command", async () => {
		vi.useFakeTimers();
		try {
			create();
			const watcher = rulesWatcherAt("/rules");
			assert.ok(watcher, "selected rules folder has no scoped watcher");
			for (let index = 0; index < 50; index++) {
				watcher.fire("change", fileUri(`/rules/part${index}.cwt`));
			}
			await vi.advanceTimersByTimeAsync(500);
			assert.strictEqual(sendRequest.mock.calls.length, 1);
			assert.deepStrictEqual(sendRequest.mock.calls[0], [
				requestType,
				{ command: "reloadrulesconfig", arguments: [] },
			]);
			assert.strictEqual(sendNotification.mock.calls.length, 50);
		} finally {
			vi.useRealTimers();
		}
	});

	test("autoReload false forwards file changes without running a reload", async () => {
		vi.useFakeTimers();
		try {
			configurationValues.set("rules.autoReload", false);
			create();
			const watcher = rulesWatcherAt("/rules");
			assert.ok(watcher);
			watcher.fire("change", fileUri("/rules/test.cwt"));
			await vi.advanceTimersByTimeAsync(1_000);
			assert.strictEqual(sendRequest.mock.calls.length, 0);
			assert.strictEqual(sendNotification.mock.calls.length, 1);
		} finally {
			vi.useRealTimers();
		}
	});

	test("workspace rules reload from the global watcher without duplicate forwarding", async () => {
		vi.useFakeTimers();
		try {
			create();
			resolveRulesCache.mockResolvedValue({
				rulesCache: "/workspace/Config",
				fetchUpstream: false,
			});
			configurationChangeHandler()(
				configurationChangeEvent(["cwtools.rules_folder"]),
			);
			await vi.waitFor(() =>
				assert.strictEqual(lastSettingsPayload().settings.rulesCache, "/workspace/Config"),
			);
			assert.strictEqual(rulesWatcherAt("/workspace/Config"), undefined);
			const forwarded = sendNotification.mock.calls.length;
			const globalWatcher = createdWatchers.find(
				(watcher) => watcher.glob === "**/*.cwt",
			);
			assert.ok(globalWatcher);
			globalWatcher.fire("change", fileUri("/workspace/Other/test.cwt"));
			await vi.advanceTimersByTimeAsync(500);
			assert.strictEqual(sendRequest.mock.calls.length, 0);
			globalWatcher.fire("change", fileUri("/workspace/Config/test.cwt"));
			await vi.advanceTimersByTimeAsync(500);
			assert.strictEqual(sendRequest.mock.calls.length, 1);
			assert.strictEqual(sendNotification.mock.calls.length, forwarded);
		} finally {
			vi.useRealTimers();
		}
	});

	test("rules folder changes swap the external watcher and cancel its pending timer", async () => {
		vi.useFakeTimers();
		const rulesRoot = fs.mkdtempSync(path.join(os.tmpdir(), "cwtools-rules-"));
		resolveRulesCache.mockResolvedValue({
			rulesCache: rulesRoot,
			fetchUpstream: false,
		});
		const changed: string[] = [];
		try {
			const { context } = create(
				() => {},
				(value) => changed.push(value),
			);
			const oldWatcher = rulesWatcherAt("/rules");
			assert.ok(oldWatcher);
			oldWatcher.fire("change", fileUri("/rules/pending.cwt"));
			configurationValues.set("rules_folder", rulesRoot);
			const initializationOptions = lastClientOptions.value
				?.initializationOptions as () => { rulesCache: string };
			configurationChangeHandler()(
				configurationChangeEvent(["cwtools.rules_folder"]),
			);
			await vi.waitFor(() => assert.deepStrictEqual(changed, [rulesRoot]));
			assert.strictEqual(oldWatcher.dispose.mock.calls.length, 1);
			const newWatcher = rulesWatcherAt(rulesRoot);
			assert.ok(newWatcher, "new resolved rules folder is not watched");
			await vi.advanceTimersByTimeAsync(500);
			assert.strictEqual(
				sendRequest.mock.calls.length,
				0,
				"old timer survived path change",
			);
			const payload = lastSettingsPayload();
			assert.strictEqual(payload.settings.rulesCache, rulesRoot);
			assert.strictEqual(initializationOptions().rulesCache, rulesRoot);
			newWatcher.fire("change", fileUri(`${rulesRoot}/rules.cwt`));
			await vi.advanceTimersByTimeAsync(500);
			assert.strictEqual(sendRequest.mock.calls.length, 1);

			stateChangeHandlers[0]?.({ oldState: 2, newState: 0 });
			assert.strictEqual(newWatcher.dispose.mock.calls.length, 1);
			stateChangeHandlers[0]?.({ oldState: 0, newState: 1 });
			const restartedWatcher = rulesWatcherAt(rulesRoot);
			assert.ok(restartedWatcher && restartedWatcher !== newWatcher);
			assert.strictEqual(initializationOptions().rulesCache, rulesRoot);

			for (const subscription of context.subscriptions) {
				subscription.dispose();
			}
			assert.strictEqual(restartedWatcher.dispose.mock.calls.length, 1);
		} finally {
			vi.useRealTimers();
			fs.rmSync(rulesRoot, { recursive: true, force: true });
		}
	});

	test("clearing custom rules selects an empty upstream cache before fetching it", async () => {
		const temporaryRoot = fs.mkdtempSync(
			path.join(os.tmpdir(), "cwtools-upstream-rules-"),
		);
		const fallbackCache = path.join(temporaryRoot, "hoi4");
		const changed: string[] = [];
		const fetchRules = vi.fn(() => {
			const payload = lastSettingsPayload();
			assert.strictEqual(payload.settings.rulesCache, fallbackCache);
			assert.deepStrictEqual(fs.readdirSync(fallbackCache), []);
		});
		resolveRulesCache.mockImplementation(async () => {
			await fs.promises.mkdir(fallbackCache, { recursive: true });
			assert.deepStrictEqual(fs.readdirSync(fallbackCache), []);
			return { rulesCache: fallbackCache, fetchUpstream: true };
		});
		try {
			configurationValues.set("rules_folder", "/custom-rules");
			create(
				() => {},
				(value) => changed.push(value),
				fetchRules,
			);
			configurationValues.delete("rules_folder");
			configurationChangeHandler()(
				configurationChangeEvent(["cwtools.rules_folder"]),
			);
			await vi.waitFor(() => assert.deepStrictEqual(changed, [fallbackCache]));

			assert.strictEqual(fetchRules.mock.calls.length, 1);
			assert.ok(rulesWatcherAt(fallbackCache));
			const payload = lastSettingsPayload();
			assert.strictEqual(payload.settings.rulesCache, fallbackCache);
		} finally {
			fs.rmSync(temporaryRoot, { recursive: true, force: true });
		}
	});

	test("ignores an in-flight rules-folder resolution after disposal", async () => {
		let finishResolution!: (setup: {
			rulesCache: string;
			fetchUpstream: boolean;
		}) => void;
		resolveRulesCache.mockImplementation(
			() =>
				new Promise((resolve) => {
					finishResolution = resolve;
				}),
		);
		const changed: string[] = [];
		const fetchRules = vi.fn();
		const { context } = create(
			() => {},
			(value) => changed.push(value),
			fetchRules,
		);
		configurationChangeHandler()(
			configurationChangeEvent(["cwtools.rules_folder"]),
		);
		await vi.waitFor(() =>
			assert.strictEqual(typeof finishResolution, "function"),
		);
		for (const subscription of context.subscriptions) subscription.dispose();
		finishResolution({ rulesCache: "/upstream", fetchUpstream: true });
		for (let tick = 0; tick < 4; tick++) await Promise.resolve();

		assert.deepStrictEqual(changed, []);
		assert.strictEqual(fetchRules.mock.calls.length, 0);
		assert.strictEqual(sendNotification.mock.calls.length, 0);
		assert.strictEqual(rulesWatcherAt("/upstream"), undefined);
	});

	test("server stop disposes the rules watcher and pending reload timer", async () => {
		vi.useFakeTimers();
		try {
			create();
			const watcher = rulesWatcherAt("/rules");
			assert.ok(watcher);
			watcher.fire("change", fileUri("/rules/pending.cwt"));
			stateChangeHandlers[0]?.({ oldState: 2, newState: 0 });
			assert.strictEqual(watcher.dispose.mock.calls.length, 1);
			await vi.advanceTimersByTimeAsync(1_000);
			assert.strictEqual(sendRequest.mock.calls.length, 0);
		} finally {
			vi.useRealTimers();
		}
	});

	async function forwardedWatchedEvents(uris: string[]): Promise<string[]> {
		const forwarded: string[] = [];
		const next = (event: { uri: string }): Promise<void> => {
			forwarded.push(event.uri);
			return Promise.resolve();
		};
		const middleware =
			lastClientOptions.value?.middleware?.workspace?.didChangeWatchedFile;
		assert.ok(middleware, "no didChangeWatchedFile middleware");
		for (const uri of uris) {
			await middleware(watchedFileEvent(uri), next);
		}
		return forwarded;
	}

	test("holds back events for files the server's own walk skips", async () => {
		create();
		assert.deepStrictEqual(
			await forwardedWatchedEvents([
				"file:///workspace/Changelog.txt",
				"file:///workspace/dist/bundle.js.map",
				"file:///workspace/common/ideas/x.txt",
				"file:///workspace/My%20Mod/events/y.txt",
			]),
			[
				"file:///workspace/common/ideas/x.txt",
				"file:///workspace/My%20Mod/events/y.txt",
			],
		);
	});

	test("measures excluded directories from the served root, not above it (#835)", async () => {
		create(undefined, undefined, undefined, {
			workspaceRoot: "/home/u/.claude/worktrees/mod",
		});
		assert.deepStrictEqual(
			await forwardedWatchedEvents([
				"file:///home/u/.claude/worktrees/mod/common/ideas/x.txt",
				"file:///home/u/.claude/worktrees/mod/target/x.txt",
				"file:///home/u/.claude/worktrees/mod/.git/x.txt",
				"file:///home/u/.claude/worktrees/mod/Changelog.txt",
			]),
			["file:///home/u/.claude/worktrees/mod/common/ideas/x.txt"],
		);
	});

	test("applies only the file-name check outside the served root", async () => {
		create();
		assert.deepStrictEqual(
			await forwardedWatchedEvents([
				"file:///other/dist/common/x.txt",
				"file:///other/Changelog.txt",
			]),
			["file:///other/dist/common/x.txt"],
		);
	});

	test("forwards rules-folder changes under an excluded directory name", () => {
		create(undefined, undefined, undefined, {
			rulesCache: "/home/u/.claude/rules",
		});
		const watcher = rulesWatcherAt("/home/u/.claude/rules");
		assert.ok(watcher, "external rules folder has no scoped watcher");
		watcher.fire("change", fileUri("/home/u/.claude/rules/a.cwt"));
		assert.deepStrictEqual(sendNotification.mock.calls, [
			[
				"workspace/didChangeWatchedFiles",
				{
					changes: [
						{ uri: "file:///home/u/.claude/rules/a.cwt", type: 2 },
					],
				},
			],
		]);
	});
});

suite("lspClient — reload settings", () => {
	beforeEach(() => {
		vi.clearAllMocks();
		lastClientOptions.value = undefined;
		configurationValues.clear();
		resolveRulesCache.mockReset();
		resolveRulesCache.mockResolvedValue({
			rulesCache: "/rules",
			fetchUpstream: false,
		});
	});

	test("prompts once for matching changes and reloads when selected", async () => {
		showInformationMessage.mockResolvedValue("Reload Window");
		create();
		const handler = configurationChangeHandler();
		handler(
			configurationChangeEvent([
				"cwtools.profiling",
				"cwtools.inlayHints.locTitles",
			]),
		);
		await Promise.resolve();

		assert.deepStrictEqual(showInformationMessage.mock.calls, [
			[
				"CWTools settings changed. Reload the window to apply them.",
				"Reload Window",
			],
		]);
		assert.deepStrictEqual(executeCommand.mock.calls, [
			["workbench.action.reloadWindow"],
		]);
	});

	test("does not reload when the prompt is dismissed", async () => {
		showInformationMessage.mockResolvedValue(undefined);
		create();
		const handler = configurationChangeHandler();
		handler(configurationChangeEvent(["cwtools.profiling"]));
		await Promise.resolve();

		assert.strictEqual(showInformationMessage.mock.calls.length, 1);
		assert.deepStrictEqual(executeCommand.mock.calls, []);
	});

	test("logs when reloading the window fails", async () => {
		const failure = new Error("reload failed");
		showInformationMessage.mockResolvedValue("Reload Window");
		executeCommand.mockRejectedValue(failure);
		create();
		const handler = configurationChangeHandler();
		handler(configurationChangeEvent(["cwtools.profiling"]));
		await vi.waitFor(() =>
			assert.deepStrictEqual(logError.mock.calls, [
				["Failed to reload window after settings change", failure],
			]),
		);
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

	// Shutdown stops the client, and the library skips the close handler for a
	// stopping client — so shutting down on an uncounted error meant a single
	// server panic was never restarted (#675).
	test("a dead pipe defers to the close handler, repeated failures shut down", async () => {
		create();
		const errorHandler = lastClientOptions.value?.errorHandler;
		assert.ok(errorHandler, "no errorHandler set on clientOptions");
		const boom = new Error("boom");
		for (const count of [undefined, 0, 1, 2, 3]) {
			const result = await errorHandler.error(boom, undefined, count);
			assert.strictEqual(
				result.action,
				1 /* ErrorAction.Continue */,
				`count ${count}`,
			);
		}
		for (const count of [4, 5]) {
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
