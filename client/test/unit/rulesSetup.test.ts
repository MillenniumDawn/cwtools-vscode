import * as assert from "assert";
import { afterEach, beforeEach, suite, test, vi } from "vitest";
import type { Memento } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";

const state = vi.hoisted(() => ({
	hasGitDirectory: false,
	head: null as string | null,
	checkoutHead: null as string | null,
	rulesFetches: [] as Array<{
		cacheDir: string;
		repo: string;
		ref: string;
		head: string | null;
	}>,
	progress: undefined as Promise<void> | undefined,
	progressStarted: undefined as Promise<void> | undefined,
	resolveProgressStarted: undefined as (() => void) | undefined,
}));

const logger = vi.hoisted(() => ({
	logError: vi.fn(),
	logInfo: vi.fn(),
	logWarn: vi.fn(),
}));

const vscode = vi.hoisted(() => ({
	showWarningMessage: vi.fn(),
}));

vi.mock("fs", () => ({
	existsSync: () => state.hasGitDirectory,
}));

vi.mock("vscode", () => ({
	ProgressLocation: { Window: 10 },
	window: {
		createOutputChannel: () => ({ appendLine: () => {} }),
		showWarningMessage: vscode.showWarningMessage,
		withProgress: (
			_options: unknown,
			task: () => Promise<void>,
		): Promise<void> => {
			const progress = task();
			state.progress = progress;
			state.resolveProgressStarted!();
			return progress;
		},
	},
	workspace: {
		getConfiguration: () => ({ get: () => undefined }),
		workspaceFolders: undefined,
	},
}));

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: {} },
}));

vi.mock("../../extension/logger", () => ({
	...logger,
	errorMessage: (err: unknown) =>
		err instanceof Error ? err.message : String(err),
}));

vi.mock("../../extension/engine", async () => {
	const { LANGUAGE_REPOS } = await import("../../extension/games");
	return {
		LANGUAGE_REPOS,
		rulesFetchCommands: (
			cacheDir: string,
			rules: { repo: string; ref: string },
			head: string | null,
		): string[][] => {
			state.rulesFetches.push({ ...rules, cacheDir, head });
			return head === rules.ref ? [] : [["checkout", rules.ref]];
		},
		runGit: (args: string[]): Promise<string> => {
			if (args.includes("rev-parse")) {
				return state.head === null
					? Promise.reject(new Error("not a git repository"))
					: Promise.resolve(`${state.head}\n`);
			}
			if (args[0] === "checkout") {
				state.hasGitDirectory = true;
				state.head = state.checkoutHead ?? args[1] ?? null;
			}
			return Promise.resolve("");
		},
	};
});

import { LANGUAGE_REPOS, RULES_MANIFEST_REVISION } from "../../extension/games";
import {
	RULES_MANIFEST_CACHE_KEY,
	RULES_MANIFEST_MAX_BYTES,
	RULES_MANIFEST_TIMEOUT_MS,
	RULES_MANIFEST_URL,
	type RulesManifest,
} from "../../extension/rulesManifest";
import { fetchRulesInBackground } from "../../extension/rulesSetup";

function manifest(
	ref: string,
	revision = RULES_MANIFEST_REVISION,
): RulesManifest {
	return {
		schema: 1,
		revision,
		pins: {
			...Object.fromEntries(
				Object.entries(LANGUAGE_REPOS).map(([id, rules]) => [id, rules.ref]),
			),
			hoi4: ref,
		},
	};
}

function memento(
	cached?: RulesManifest,
	updateFailure?: Error,
): {
	globalState: Memento;
	updates: Array<[string, unknown]>;
} {
	let stored = cached;
	const updates: Array<[string, unknown]> = [];
	return {
		globalState: {
			get<T>(key: string): T | undefined {
				return key === RULES_MANIFEST_CACHE_KEY ? (stored as T) : undefined;
			},
			update(key: string, value: unknown): Promise<void> {
				updates.push([key, value]);
				if (updateFailure) return Promise.reject(updateFailure);
				stored = value as RulesManifest;
				return Promise.resolve();
			},
		} as unknown as Memento,
		updates,
	};
}

function client(): { client: LanguageClient; requests: unknown[] } {
	const requests: unknown[] = [];
	return {
		client: {
			sendRequest: (_type: unknown, params: unknown): Promise<void> => {
				requests.push(params);
				return Promise.resolve();
			},
		} as unknown as LanguageClient,
		requests,
	};
}

function stubManifestFetch(value: RulesManifest | string) {
	const text = typeof value === "string" ? value : JSON.stringify(value);
	const fetch = vi.fn().mockResolvedValue(
		new Response(text, {
			status: 200,
			headers: {
				"content-length": String(new TextEncoder().encode(text).length),
			},
		}),
	);
	vi.stubGlobal("fetch", fetch);
	return fetch;
}

async function waitForProgress(): Promise<void> {
	await state.progressStarted;
	await state.progress;
}

suite("rulesSetup — reviewed manifest sync", () => {
	beforeEach(() => {
		vi.clearAllMocks();
		vi.unstubAllGlobals();
		state.hasGitDirectory = false;
		state.head = null;
		state.checkoutHead = null;
		state.rulesFetches.length = 0;
		state.progress = undefined;
		state.progressStarted = new Promise<void>((resolve) => {
			state.resolveProgressStarted = resolve;
		});
	});

	afterEach(() => vi.unstubAllGlobals());

	test("persists a newer manifest, fetches its pin, and reloads after the initial scan", async () => {
		const ref = "f".repeat(40);
		const reviewed = manifest(ref, RULES_MANIFEST_REVISION + 1);
		const fetch = stubManifestFetch(reviewed);
		const { globalState, updates } = memento();
		const { client: languageClient, requests } = client();
		let finishInitialScan!: () => void;
		const initialScanDone = new Promise<void>((resolve) => {
			finishInitialScan = resolve;
		});

		fetchRulesInBackground(
			"hoi4",
			"/cache",
			languageClient,
			initialScanDone,
			globalState,
		);

		await state.progressStarted;
		assert.strictEqual(fetch.mock.calls[0]?.[0], RULES_MANIFEST_URL);
		assert.deepStrictEqual(updates, [[RULES_MANIFEST_CACHE_KEY, reviewed]]);
		assert.deepStrictEqual(state.rulesFetches, [
			{
				cacheDir: "/cache/hoi4",
				repo: LANGUAGE_REPOS.hoi4.repo,
				ref,
				head: null,
			},
		]);
		assert.deepStrictEqual(requests, []);

		finishInitialScan();
		await state.progress;
		assert.deepStrictEqual(logger.logError.mock.calls, []);
		assert.deepStrictEqual(requests, [
			{ command: "reloadrulesconfig", arguments: [] },
		]);
	});

	test("uses a newer manifest when caching it fails", async () => {
		const ref = "f".repeat(40);
		const reviewed = manifest(ref, RULES_MANIFEST_REVISION + 1);
		stubManifestFetch(reviewed);
		const { globalState, updates } = memento(undefined, new Error("disk full"));
		const { client: languageClient } = client();

		fetchRulesInBackground(
			"hoi4",
			"/cache",
			languageClient,
			Promise.resolve(),
			globalState,
		);

		await waitForProgress();
		assert.deepStrictEqual(updates, [[RULES_MANIFEST_CACHE_KEY, reviewed]]);
		assert.strictEqual(state.rulesFetches[0]?.ref, ref);
	});

	test("keeps a cached pin when the remote manifest is invalid", async () => {
		const cached = manifest("c".repeat(40), RULES_MANIFEST_REVISION + 1);
		stubManifestFetch("{");
		const { globalState, updates } = memento(cached);
		const { client: languageClient, requests } = client();

		fetchRulesInBackground(
			"hoi4",
			"/cache",
			languageClient,
			Promise.resolve(),
			globalState,
		);

		await waitForProgress();
		assert.deepStrictEqual(updates, []);
		assert.strictEqual(state.rulesFetches[0]?.ref, cached.pins.hoi4);
		assert.deepStrictEqual(logger.logError.mock.calls, []);
		assert.deepStrictEqual(requests, [
			{ command: "reloadrulesconfig", arguments: [] },
		]);
	});

	test("keeps a newer cached pin when the remote manifest is stale", async () => {
		const cached = manifest("c".repeat(40), RULES_MANIFEST_REVISION + 2);
		stubManifestFetch(manifest("d".repeat(40), RULES_MANIFEST_REVISION + 1));
		const { globalState, updates } = memento(cached);
		const { client: languageClient } = client();

		fetchRulesInBackground(
			"hoi4",
			"/cache",
			languageClient,
			Promise.resolve(),
			globalState,
		);

		await waitForProgress();
		assert.deepStrictEqual(updates, []);
		assert.strictEqual(state.rulesFetches[0]?.ref, cached.pins.hoi4);
	});

	test("keeps a cached pin when the remote revision conflicts", async () => {
		const cached = manifest("c".repeat(40), RULES_MANIFEST_REVISION + 1);
		stubManifestFetch(manifest("d".repeat(40), RULES_MANIFEST_REVISION + 1));
		const { globalState, updates } = memento(cached);
		const { client: languageClient } = client();

		fetchRulesInBackground(
			"hoi4",
			"/cache",
			languageClient,
			Promise.resolve(),
			globalState,
		);

		await waitForProgress();
		assert.deepStrictEqual(updates, []);
		assert.strictEqual(state.rulesFetches[0]?.ref, cached.pins.hoi4);
	});

	test("uses the bundled pin when no valid manifest is available", async () => {
		stubManifestFetch("{");
		const { globalState } = memento();
		const { client: languageClient } = client();

		fetchRulesInBackground(
			"hoi4",
			"/cache",
			languageClient,
			Promise.resolve(),
			globalState,
		);

		await waitForProgress();
		assert.strictEqual(state.rulesFetches[0]?.ref, LANGUAGE_REPOS.hoi4.ref);
	});

	test("uses the bundled pin without reading an oversized response", async () => {
		const reviewed = manifest("f".repeat(40), RULES_MANIFEST_REVISION + 1);
		const text = JSON.stringify(reviewed);
		let readerRequests = 0;
		const body = {
			getReader: () => {
				readerRequests++;
				return new ReadableStream<Uint8Array>({
					start(controller) {
						controller.enqueue(new TextEncoder().encode(text));
						controller.close();
					},
				}).getReader();
			},
		} as unknown as ReadableStream<Uint8Array>;
		vi.stubGlobal(
			"fetch",
			vi.fn().mockResolvedValue({
				ok: true,
				status: 200,
				headers: new Headers({
					"content-length": String(RULES_MANIFEST_MAX_BYTES + 1),
				}),
				body,
			}),
		);
		const { globalState } = memento();
		const { client: languageClient } = client();

		fetchRulesInBackground(
			"hoi4",
			"/cache",
			languageClient,
			Promise.resolve(),
			globalState,
		);

		await waitForProgress();
		assert.strictEqual(readerRequests, 0);
		assert.strictEqual(state.rulesFetches[0]?.ref, LANGUAGE_REPOS.hoi4.ref);
	});

	test("uses a cached pin after the manifest request times out", async () => {
		vi.useFakeTimers();
		try {
			let signal: AbortSignal | null | undefined;
			vi.stubGlobal(
				"fetch",
				vi.fn(
					(_url: string, init?: RequestInit) =>
						new Promise<Response>((_resolve, reject) => {
							signal = init?.signal;
							signal?.addEventListener(
								"abort",
								() => reject(new Error("aborted")),
								{ once: true },
							);
						}),
				),
			);
			const cached = manifest("c".repeat(40), RULES_MANIFEST_REVISION + 1);
			const { globalState, updates } = memento(cached);
			const { client: languageClient } = client();

			fetchRulesInBackground(
				"hoi4",
				"/cache",
				languageClient,
				Promise.resolve(),
				globalState,
			);

			await vi.advanceTimersByTimeAsync(RULES_MANIFEST_TIMEOUT_MS);
			await waitForProgress();
			assert.strictEqual(signal?.aborted, true);
			assert.deepStrictEqual(updates, []);
			assert.strictEqual(state.rulesFetches[0]?.ref, cached.pins.hoi4);
		} finally {
			vi.useRealTimers();
		}
	});

	test("does not reload rules when checkout lands on another commit", async () => {
		const ref = "f".repeat(40);
		state.checkoutHead = "e".repeat(40);
		stubManifestFetch(manifest(ref, RULES_MANIFEST_REVISION + 1));
		const { globalState } = memento();
		const { client: languageClient, requests } = client();

		fetchRulesInBackground(
			"hoi4",
			"/cache",
			languageClient,
			Promise.resolve(),
			globalState,
		);

		await waitForProgress();
		assert.deepStrictEqual(requests, []);
		assert.ok(vscode.showWarningMessage.mock.calls.length > 0);
	});
});
