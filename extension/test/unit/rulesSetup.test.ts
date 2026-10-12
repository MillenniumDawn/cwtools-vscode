import * as assert from "assert";
import * as fsPromises from "fs/promises";
import * as os from "os";
import * as path from "path";
import { afterEach, beforeEach, suite, test, vi } from "vitest";
import type { Memento } from "vscode";
import type * as VscodeStub from "./_stubs/vscode";
import type { LanguageClient } from "vscode-languageclient/node";

// The rules-sync host label clones the pinned commit for real. These cover the
// decisions around it that need a failing git or a hostile manifest: which pin
// wins, when the server is told to reload, and which warning the user sees.

const state = vi.hoisted(() => ({
	hasGitDirectory: false,
	head: null as string | null,
	checkoutHead: null as string | null,
	gitMissing: false,
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
	showInformationMessage: vi.fn(),
}));

vi.mock("fs", () => ({
	existsSync: () => state.hasGitDirectory,
}));

vi.mock("vscode", async (importOriginal) => {
	const original = await importOriginal<typeof VscodeStub>();
	return {
		...original,
		window: {
			...original.window,
			showWarningMessage: vscode.showWarningMessage,
			showInformationMessage: vscode.showInformationMessage,
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
	};
});

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: {} },
}));

vi.mock("../../src/host/logger", async () =>
	(await import("./support/loggerMock")).mockLogger(logger),
);

vi.mock("../../src/host/engine", async () => {
	const { LANGUAGE_REPOS } = await import("../../src/host/games");
	class GitNotFoundError extends Error {
		constructor() {
			super("git was not found on your PATH; install Git or add it to PATH");
			this.name = "GitNotFoundError";
		}
	}
	return {
		LANGUAGE_REPOS,
		GitNotFoundError,
		resolveRulesFolder: (raw: string | undefined) => ({
			path: raw?.trim() || undefined,
			existed: false,
		}),
		rulesFetchCommands: (
			cacheDir: string,
			rules: { repo: string; ref: string },
			head: string | null,
		): string[][] => {
			state.rulesFetches.push({ ...rules, cacheDir, head });
			return head === rules.ref ? [] : [["checkout", rules.ref]];
		},
		runGit: (args: string[]): Promise<string> => {
			// A missing git binary fails every invocation, including rev-parse;
			// the real runGit has no rev-parse special case.
			if (state.gitMissing) {
				return Promise.reject(new GitNotFoundError());
			}
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

import { LANGUAGE_REPOS, RULES_MANIFEST_REVISION } from "../../src/host/games";
import {
	RULES_MANIFEST_CACHE_KEY,
	RULES_MANIFEST_TIMEOUT_MS,
	RULES_MANIFEST_URL,
	type RulesManifest,
} from "../../src/host/rulesManifest";
import {
	fetchRulesInBackground,
	resolveRulesCache,
} from "../../src/host/rulesSetup";

const NEXT = RULES_MANIFEST_REVISION + 1;
const RELOAD = [{ command: "reloadrulesconfig", arguments: [] }];
const sha = (letter: string) => letter.repeat(40);

function manifest(ref: string, revision = NEXT): RulesManifest {
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

function stubManifestFetch(value: RulesManifest | string) {
	const text = typeof value === "string" ? value : JSON.stringify(value);
	const fetch = vi.fn().mockImplementation(() =>
		Promise.resolve(
			new Response(text, {
				status: 200,
				headers: {
					"content-length": String(new TextEncoder().encode(text).length),
				},
			}),
		),
	);
	vi.stubGlobal("fetch", fetch);
	return fetch;
}

// One sync against a fake memento and client, recording what each was told.
function session(cached?: RulesManifest) {
	let stored = cached;
	const updates: Array<[string, unknown]> = [];
	const requests: unknown[] = [];
	const globalState = {
		get: (key: string) =>
			key === RULES_MANIFEST_CACHE_KEY ? stored : undefined,
		update(key: string, value: unknown): Promise<void> {
			updates.push([key, value]);
			stored = value as RulesManifest;
			return Promise.resolve();
		},
	} as unknown as Memento;
	const languageClient = {
		sendRequest: (_type: unknown, params: unknown): Promise<void> => {
			requests.push(params);
			return Promise.resolve();
		},
	} as unknown as LanguageClient;
	return {
		updates,
		requests,
		sync: (initialScanDone: Promise<void> = Promise.resolve()) =>
			fetchRulesInBackground(
				"hoi4",
				"/cache",
				languageClient,
				initialScanDone,
				globalState,
			),
	};
}

async function settle(pending: Promise<unknown>): Promise<void> {
	await pending;
	await state.progressStarted;
	await state.progress;
}

const warnings = () =>
	vscode.showWarningMessage.mock.calls.map((call) => call[0] as string);

suite("rulesSetup", () => {
	beforeEach(() => {
		vi.unstubAllGlobals();
		state.hasGitDirectory = false;
		state.head = null;
		state.checkoutHead = null;
		state.gitMissing = false;
		state.rulesFetches.length = 0;
		state.progress = undefined;
		state.progressStarted = new Promise<void>((resolve) => {
			state.resolveProgressStarted = resolve;
		});
	});

	afterEach(() => {
		vi.useRealTimers();
		vi.unstubAllGlobals();
	});

	test("invalid custom rules fall back to an empty upstream cache with a warning", async () => {
		const temporaryRoot = await fsPromises.mkdtemp(
			path.join(os.tmpdir(), "cwtools-invalid-rules-"),
		);
		const cacheDir = path.join(temporaryRoot, "cache");
		const invalidRules = path.join(temporaryRoot, "missing-rules");
		vi.stubEnv("CWTOOLS_TEST_RULES_FOLDER", invalidRules);
		try {
			const setup = await resolveRulesCache("hoi4", cacheDir, temporaryRoot);
			assert.strictEqual(setup.rulesCache, path.join(cacheDir, "hoi4"));
			assert.strictEqual(setup.fetchUpstream, true);
			assert.deepStrictEqual(await fsPromises.readdir(setup.rulesCache), []);
			assert.deepStrictEqual(warnings(), [
				`CWTools: the rules_folder "${invalidRules}" could not be found (tried "${invalidRules}"). Falling back to the bundled/upstream rules.`,
			]);
		} finally {
			vi.unstubAllEnvs();
			await fsPromises.rm(temporaryRoot, { recursive: true, force: true });
		}
	});

	test("persists a newer manifest, fetches its pin once, and reloads after the initial scan", async () => {
		const reviewed = manifest(sha("f"));
		const fetch = stubManifestFetch(reviewed);
		const { sync, updates, requests } = session();
		let finishInitialScan!: () => void;
		const initialScanDone = new Promise<void>((resolve) => {
			finishInitialScan = resolve;
		});

		const first = sync(initialScanDone);
		assert.strictEqual(sync(initialScanDone), first, "in-flight fetch is shared");
		await state.progressStarted;

		assert.strictEqual(fetch.mock.calls[0]?.[0], RULES_MANIFEST_URL);
		assert.deepStrictEqual(updates, [[RULES_MANIFEST_CACHE_KEY, reviewed]]);
		assert.deepStrictEqual(state.rulesFetches, [
			{
				cacheDir: "/cache/hoi4",
				repo: LANGUAGE_REPOS.hoi4.repo,
				ref: sha("f"),
				head: null,
			},
		]);
		assert.deepStrictEqual(requests, []);

		finishInitialScan();
		await settle(first);
		assert.deepStrictEqual(logger.logError.mock.calls, []);
		assert.deepStrictEqual(requests, RELOAD);
		// A first-time download has nothing to replace, so it stays silent.
		assert.deepStrictEqual(vscode.showInformationMessage.mock.calls, []);

		await sync();
		assert.strictEqual(state.rulesFetches.length, 2, "a later sync runs again");
	});

	test("names both commits and notifies when a bump replaces an existing cache", async () => {
		state.hasGitDirectory = true;
		state.head = sha("c");
		stubManifestFetch(manifest(sha("f")));
		const { sync, requests } = session();

		await settle(sync());

		assert.deepStrictEqual(requests, RELOAD);
		assert.strictEqual(vscode.showInformationMessage.mock.calls.length, 1);
		const info = vscode.showInformationMessage.mock.calls[0][0] as string;
		assert.ok(info.includes(sha("c")) && info.includes(sha("f")), info);
	});

	test.each<[string, RulesManifest | undefined, RulesManifest | string, string]>([
		["a cached pin beats an invalid remote manifest", manifest(sha("c")), "{", sha("c")],
		["a newer cached pin beats a stale remote manifest", manifest(sha("c"), NEXT + 1), manifest(sha("d")), sha("c")],
		["a cached pin beats a conflicting remote at the same revision", manifest(sha("c")), manifest(sha("d")), sha("c")],
		["the bundled pin is used when no valid manifest exists", undefined, "{", LANGUAGE_REPOS.hoi4.ref],
	])("%s", async (_name, cached, remote, expectedRef) => {
		stubManifestFetch(remote);
		const { sync, updates } = session(cached);

		await settle(sync());

		assert.deepStrictEqual(updates, []);
		assert.strictEqual(state.rulesFetches[0]?.ref, expectedRef);
		assert.deepStrictEqual(logger.logError.mock.calls, []);
	});

	test("uses a cached pin after the manifest request times out", async () => {
		vi.useFakeTimers();
		let signal: AbortSignal | null | undefined;
		vi.stubGlobal(
			"fetch",
			vi.fn(
				(_url: string, init?: RequestInit) =>
					new Promise<Response>((_resolve, reject) => {
						signal = init?.signal;
						signal?.addEventListener("abort", () => reject(new Error("aborted")), {
							once: true,
						});
					}),
			),
		);
		const cached = manifest(sha("c"));
		const { sync, updates } = session(cached);

		const pending = sync();
		await vi.advanceTimersByTimeAsync(RULES_MANIFEST_TIMEOUT_MS);
		await settle(pending);

		assert.strictEqual(signal?.aborted, true);
		assert.deepStrictEqual(updates, []);
		assert.strictEqual(state.rulesFetches[0]?.ref, cached.pins.hoi4);
	});

	test("warns about missing git, not the network, on an initial clone", async () => {
		state.gitMissing = true;
		stubManifestFetch(manifest(sha("f")));
		const { sync, requests } = session();

		await settle(sync());

		assert.deepStrictEqual(requests, []);
		assert.strictEqual(warnings().length, 1);
		assert.match(warnings()[0], /CWTools needs Git on your PATH/);
		assert.doesNotMatch(warnings()[0], /check your network/);
	});

	test("a checkout that lands on another commit warns and does not reload", async () => {
		state.checkoutHead = sha("e");
		stubManifestFetch(manifest(sha("f")));
		const { sync, requests } = session();

		await settle(sync());

		assert.deepStrictEqual(requests, []);
		assert.strictEqual(warnings().length, 1);
		assert.match(warnings()[0], /check your network/);
		assert.match(warnings()[0], /Rules cache landed on/);
	});

	// The previous pin is still on disk, so a failed bump is not worth a toast.
	test("a failed bump with rules already present stays log-only", async () => {
		state.hasGitDirectory = true;
		state.head = sha("e");
		state.checkoutHead = sha("a");
		stubManifestFetch(manifest(sha("f")));
		const { sync, requests } = session();

		await settle(sync());

		assert.deepStrictEqual(requests, []);
		assert.deepStrictEqual(warnings(), []);
	});
});
