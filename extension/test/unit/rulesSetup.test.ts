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
	hostileResponse: undefined as
		| "declared-oversized"
		| "streamed-oversized"
		| undefined,
	streamedBodyCancelled: false,
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
	RULES_MANIFEST_ETAG_CACHE_KEY,
	RULES_MANIFEST_MAX_BYTES,
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

function stubManifestFetch(value: RulesManifest | string, etag?: string) {
	const text = typeof value === "string" ? value : JSON.stringify(value);
	const headers: Record<string, string> = {
		"content-length": String(new TextEncoder().encode(text).length),
	};
	if (etag) {
		headers.etag = etag;
	}
	const fetch = vi.fn().mockImplementation(() => {
		if (state.hostileResponse === "declared-oversized") {
			return Promise.resolve(
				new Response("{}", {
					status: 200,
					headers: {
						"content-length": String(RULES_MANIFEST_MAX_BYTES + 1),
					},
				}),
			);
		}
		if (state.hostileResponse === "streamed-oversized") {
			const body = new ReadableStream<Uint8Array>({
				start(controller) {
					controller.enqueue(
						new Uint8Array(RULES_MANIFEST_MAX_BYTES + 1),
					);
				},
				cancel() {
					state.streamedBodyCancelled = true;
				},
			});
			return Promise.resolve(
				new Response(body, {
					status: 200,
					headers: { "content-length": "1" },
				}),
			);
		}
		return Promise.resolve(
			new Response(text, {
				status: 200,
				headers,
			}),
		);
	});
	vi.stubGlobal("fetch", fetch);
	return fetch;
}

// One sync against a fake memento and client, recording what each was told.
function session(
	cached?: RulesManifest,
	manifestUpdateError?: Error,
	cachedEtag?: string,
) {
	let stored = cached;
	let etag = cachedEtag;
	const updates: Array<[string, unknown]> = [];
	const requests: unknown[] = [];
	const globalState = {
		get: (key: string) => {
			if (key === RULES_MANIFEST_CACHE_KEY) return stored;
			if (key === RULES_MANIFEST_ETAG_CACHE_KEY) return etag;
			return undefined;
		},
		update(key: string, value: unknown): Promise<void> {
			updates.push([key, value]);
			if (manifestUpdateError && key === RULES_MANIFEST_CACHE_KEY) {
				return Promise.reject(manifestUpdateError);
			}
			if (key === RULES_MANIFEST_CACHE_KEY) {
				stored = value as RulesManifest;
			} else if (key === RULES_MANIFEST_ETAG_CACHE_KEY) {
				etag = value as string | undefined;
			}
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
		state.hostileResponse = undefined;
		state.streamedBodyCancelled = false;
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

	test("uses a newer manifest when caching it fails", async () => {
		const reviewed = manifest(sha("f"));
		stubManifestFetch(reviewed);
		const { sync, updates } = session(undefined, new Error("disk full"));

		await settle(sync());

		assert.deepStrictEqual(updates, [[RULES_MANIFEST_CACHE_KEY, reviewed]]);
		assert.strictEqual(state.rulesFetches[0]?.ref, sha("f"));
		assert.deepStrictEqual(logger.logWarn.mock.calls, [
			["Could not cache rules manifest: disk full"],
		]);
	});

	test("skips an oversized response by content-length without reading the body", async () => {
		vi.useFakeTimers();
		const response = new Response(JSON.stringify(manifest(sha("f"))), {
			status: 200,
			headers: { "content-length": String(RULES_MANIFEST_MAX_BYTES + 1) },
		});
		vi.stubGlobal("fetch", vi.fn().mockResolvedValue(response));
		const { sync, updates } = session();

		await settle(sync());

		assert.strictEqual(response.bodyUsed, false);
		assert.deepStrictEqual(updates, []);
		assert.strictEqual(state.rulesFetches[0]?.ref, LANGUAGE_REPOS.hoi4.ref);
		assert.strictEqual(vi.getTimerCount(), 0, "the fetch timeout should be cleared");
	});

	test.each<
		[
			string,
			"declared-oversized" | "streamed-oversized",
			RulesManifest | undefined,
			string,
		]
	>([
		[
			"declared oversized response preserves the cached pin",
			"declared-oversized",
			manifest(sha("c")),
			sha("c"),
		],
		[
			"streamed oversized response uses the bundled pin",
			"streamed-oversized",
			undefined,
			LANGUAGE_REPOS.hoi4.ref,
		],
		[
			"streamed oversized response preserves the cached pin",
			"streamed-oversized",
			manifest(sha("c")),
			sha("c"),
		],
	])("%s", async (_name, hostileResponse, cached, expectedRef) => {
		vi.useFakeTimers();
		state.hostileResponse = hostileResponse;
		stubManifestFetch(manifest(sha("f")));
		const { sync, updates } = session(cached);

		await settle(sync());

		assert.deepStrictEqual(updates, [], "an oversized manifest must not be cached");
		assert.strictEqual(state.rulesFetches[0]?.ref, expectedRef);
		assert.ok(
			logger.logWarn.mock.calls.some(([message]) =>
				String(message).includes(
					`Rules manifest refresh failed; using ${cached ? "cached" : "bundled"} pins: Rules manifest response is too large.`,
				),
			),
			"the rejected remote manifest should leave the cached or bundled pin in use",
		);
		assert.strictEqual(
			state.streamedBodyCancelled,
			hostileResponse === "streamed-oversized",
		);
		assert.strictEqual(vi.getTimerCount(), 0, "the fetch timeout should be cleared");
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

	const ifNoneMatchHeader = (call: unknown[] | undefined): unknown =>
		(call?.[1] as RequestInit | undefined)?.headers;

	test("a 304 keeps the cached manifest without rewriting it or warning", async () => {
		const cached = manifest(sha("c"));
		const etag = '"etag-1"';
		const fetch = vi
			.fn()
			.mockResolvedValue(new Response(null, { status: 304 }));
		vi.stubGlobal("fetch", fetch);
		const { sync, updates } = session(cached, undefined, etag);

		await settle(sync());

		assert.strictEqual(fetch.mock.calls[0]?.[0], RULES_MANIFEST_URL);
		assert.deepStrictEqual(ifNoneMatchHeader(fetch.mock.calls[0]), {
			"If-None-Match": etag,
		});
		assert.deepStrictEqual(updates, [], "a 304 must not rewrite the cache");
		assert.deepStrictEqual(logger.logWarn.mock.calls, []);
		assert.strictEqual(state.rulesFetches[0]?.ref, sha("c"));
	});

	test("a 200 stores its ETag and a later request sends If-None-Match", async () => {
		const reviewed = manifest(sha("f"));
		const etag = '"etag-2"';
		const fetch = vi
			.fn()
			.mockImplementationOnce(() =>
				Promise.resolve(
					new Response(JSON.stringify(reviewed), {
						status: 200,
						headers: {
							"content-length": String(
								new TextEncoder().encode(JSON.stringify(reviewed)).length,
							),
							etag,
						},
					}),
				),
			)
			.mockImplementationOnce(() =>
				Promise.resolve(new Response(null, { status: 304 })),
			);
		vi.stubGlobal("fetch", fetch);
		const { sync, updates } = session();

		await settle(sync());
		await settle(sync());

		assert.strictEqual(
			ifNoneMatchHeader(fetch.mock.calls[0]),
			undefined,
			"the first request must fetch the body",
		);
		assert.deepStrictEqual(ifNoneMatchHeader(fetch.mock.calls[1]), {
			"If-None-Match": etag,
		});
		assert.deepStrictEqual(updates, [
			[RULES_MANIFEST_CACHE_KEY, reviewed],
			[RULES_MANIFEST_ETAG_CACHE_KEY, etag],
		]);
		assert.deepStrictEqual(logger.logWarn.mock.calls, []);
	});

	test("a 200 without an ETag clears any stored one so the body is fetched again", async () => {
		const fetched = manifest(sha("f"), NEXT + 1);
		const fetch = stubManifestFetch(fetched);
		const { sync, updates } = session(
			manifest(sha("c")),
			undefined,
			'"etag-3"',
		);

		await settle(sync());
		await settle(sync());

		assert.strictEqual(
			state.rulesFetches.length,
			2,
			"both syncs fetch the body",
		);
		assert.deepStrictEqual(ifNoneMatchHeader(fetch.mock.calls[0]), {
			"If-None-Match": '"etag-3"',
		});
		assert.strictEqual(
			ifNoneMatchHeader(fetch.mock.calls[1]),
			undefined,
			"a cleared ETag means the next request fetches the body",
		);
		assert.deepStrictEqual(updates, [
			[RULES_MANIFEST_CACHE_KEY, fetched],
			[RULES_MANIFEST_ETAG_CACHE_KEY, undefined],
		]);
	});

	test("an ETag is not stored for a manifest that could not be cached", async () => {
		const fetched = manifest(sha("f"), NEXT + 1);
		const fetch = stubManifestFetch(fetched, '"etag-new"');
		const { sync, updates } = session(
			manifest(sha("c")),
			new Error("disk full"),
			'"etag-old"',
		);

		await settle(sync());
		await settle(sync());

		assert.strictEqual(
			ifNoneMatchHeader(fetch.mock.calls[1]),
			undefined,
			"the next request must fetch the body the cache is missing",
		);
		assert.deepStrictEqual(updates, [
			[RULES_MANIFEST_CACHE_KEY, fetched],
			[RULES_MANIFEST_ETAG_CACHE_KEY, undefined],
			[RULES_MANIFEST_CACHE_KEY, fetched],
		]);
		assert.deepStrictEqual(
			state.rulesFetches.map(({ ref }) => ref),
			[sha("f"), sha("f")],
		);
	});

	test("a 304 with no usable cache falls back to the bundled pin", async () => {
		vi.stubGlobal(
			"fetch",
			vi.fn().mockResolvedValue(new Response(null, { status: 304 })),
		);
		const { sync, updates } = session();

		await settle(sync());

		assert.deepStrictEqual(updates, []);
		assert.strictEqual(state.rulesFetches[0]?.ref, LANGUAGE_REPOS.hoi4.ref);
		assert.deepStrictEqual(logger.logWarn.mock.calls, [
			[
				"Rules manifest refresh failed; using bundled pins: Rules manifest is unmodified but no usable manifest is cached.",
			],
		]);
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

	test("still warns about missing git when a stale .git directory is present", async () => {
		state.hasGitDirectory = true;
		state.head = sha("e");
		state.gitMissing = true;
		stubManifestFetch(manifest(sha("f")));
		const { sync, requests } = session();

		await settle(sync());

		assert.deepStrictEqual(requests, []);
		assert.strictEqual(warnings().length, 1);
		assert.match(warnings()[0], /CWTools needs Git on your PATH/);
		assert.doesNotMatch(warnings()[0], /check your network/);
	});

	test("a sync can be retried after a failed fetch completes", async () => {
		state.gitMissing = true;
		const reviewed = manifest(sha("f"));
		stubManifestFetch(reviewed);
		const { sync, updates, requests } = session();

		const failedAttempt = sync();
		await settle(failedAttempt);
		assert.strictEqual(state.rulesFetches.length, 1);
		assert.deepStrictEqual(requests, []);
		assert.strictEqual(warnings().length, 1);
		const errorsAfterFailure = [...logger.logError.mock.calls];

		state.gitMissing = false;
		const retry = sync();
		assert.notStrictEqual(
			retry,
			failedAttempt,
			"a settled failed fetch should be removed from the in-flight map",
		);
		await settle(retry);

		assert.strictEqual(state.rulesFetches.length, 2);
		assert.deepStrictEqual(requests, RELOAD);
		assert.strictEqual(warnings().length, 1, "the successful retry should not warn");
		assert.deepStrictEqual(
			logger.logError.mock.calls,
			errorsAfterFailure,
			"the successful retry should not add an error log",
		);
		assert.deepStrictEqual(updates, [[RULES_MANIFEST_CACHE_KEY, reviewed]]);
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
