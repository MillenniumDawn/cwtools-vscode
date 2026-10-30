import * as assert from "assert";
import {
	afterEach,
	beforeEach,
	suite,
	test,
	vi,
} from "vitest";
import type * as OsModule from "os";
import type * as FsPromisesModule from "fs/promises";
import type { RulesSetup } from "../../src/host/rulesSetup";

// Controlled promises prove overlap, wait-for-both, and both failure paths without timing or disk I/O.
const state = vi.hoisted(() => {
	type StatLike = { mode: number };
	type PendingStat = {
		resolve: (stat: StatLike) => void;
		reject: (err: unknown) => void;
	};
	const pendingStats: PendingStat[] = [];
	return {
		platform: "linux",
		stats: [] as string[],
		chmods: [] as Array<{ path: string; mode: number }>,
		chmodError: undefined as Error | undefined,
		pendingStats,
		// Settle all outstanding stat calls in order (resolve by default).
		settleStat: (stat?: StatLike, err?: unknown) => {
			for (const pending of pendingStats.splice(0)) {
				if (err !== undefined) {
					pending.reject(err);
				} else {
					pending.resolve(stat ?? { mode: 0o755 });
				}
			}
		},
	};
});

const logger = vi.hoisted(() => ({ logError: vi.fn() }));

vi.mock("os", async (importOriginal) => {
	const original = await importOriginal<typeof OsModule>();
	return { ...original, platform: () => state.platform };
});

vi.mock("fs/promises", async (importOriginal) => {
	const original = await importOriginal<typeof FsPromisesModule>();
	return {
		...original,
		stat: (p: Parameters<typeof FsPromisesModule.stat>[0]) => {
			state.stats.push(String(p));
			let resolve!: (stat: { mode: number }) => void;
			let reject!: (err: unknown) => void;
			const promise = new Promise<{ mode: number }>((res, rej) => {
				resolve = res;
				reject = rej;
			});
			state.pendingStats.push({ resolve, reject });
			return promise;
		},
		chmod: (
			p: Parameters<typeof FsPromisesModule.chmod>[0],
			mode: Parameters<typeof FsPromisesModule.chmod>[1],
		) => {
			state.chmods.push({ path: String(p), mode: Number(mode) });
			return state.chmodError === undefined
				? Promise.resolve(undefined)
				: Promise.reject(state.chmodError);
		},
	};
});

vi.mock("../../src/host/logger", async () =>
	(await import("./support/loggerMock")).mockLogger(logger),
);

import { prepareClientStart } from "../../src/host/startupPreparation";

const EXE = "/staged/cwtools-server";
const SETUP = (): RulesSetup => ({ rulesCache: "/rules", fetchUpstream: false });

// Let every pending microtask and immediate settle before asserting.
const flush = async (): Promise<void> => {
	await new Promise<void>((resolve) => setImmediate(resolve));
	await new Promise<void>((resolve) => setImmediate(resolve));
};

function settledTracker(): {
	settled: () => boolean;
	track: (p: Promise<unknown>) => void;
} {
	let isSettled = false;
	return {
		settled: () => isSettled,
		track: (p: Promise<unknown>) => {
			p.then(
				() => {
					isSettled = true;
				},
				() => {
					isSettled = true;
				},
			);
		},
	};
}

function deferred<T>(): {
	promise: Promise<T>;
	release: (value: T) => void;
} {
	let release!: (value: T) => void;
	const promise = new Promise<T>((resolve) => {
		release = resolve;
	});
	return { promise, release };
}

// Records its start synchronously, before awaiting the gate.
function gatedRules(gate: Promise<RulesSetup>): {
	calls: () => number;
	resolveRules: () => Promise<RulesSetup>;
} {
	let calls = 0;
	const resolveRules = async (): Promise<RulesSetup> => {
		calls += 1;
		return await gate;
	};
	return { calls: () => calls, resolveRules };
}

suite("prepareClientStart", () => {
	beforeEach(() => {
		state.platform = "linux";
		state.stats.length = 0;
		state.chmods.length = 0;
		state.chmodError = undefined;
	});

	afterEach(() => {
		// Never leave a deferred stat dangling behind a test.
		state.settleStat();
	});

	test("starts the binary fix-up and the rules-cache resolve together, and waits for both", async () => {
		const rulesGate = deferred<RulesSetup>();
		const rules = gatedRules(rulesGate.promise);

		const prepare = prepareClientStart(EXE, rules.resolveRules);
		const tracker = settledTracker();
		tracker.track(prepare);

		// Both operations began with neither finished: overlap, not sequence.
		assert.strictEqual(rules.calls(), 1, "the rules resolve should have started");
		assert.strictEqual(state.stats.length, 1, "the binary stat should have started");

		await flush();
		assert.strictEqual(tracker.settled(), false, "preparation should wait for both");

		// Finishing only one side leaves preparation pending.
		rulesGate.release(SETUP());
		await flush();
		assert.strictEqual(
			tracker.settled(),
			false,
			"preparation should wait for the binary fix-up",
		);
		state.settleStat();
		assert.deepStrictEqual(await prepare, SETUP());
		assert.strictEqual(state.chmods.length, 0, "no chmod once the exec bit is set");
	});

	test("waits for the rules-cache resolve after the binary fix-up has settled", async () => {
		const rulesGate = deferred<RulesSetup>();
		const rules = gatedRules(rulesGate.promise);

		const prepare = prepareClientStart(EXE, rules.resolveRules);
		const tracker = settledTracker();
		tracker.track(prepare);
		await flush();
		assert.strictEqual(rules.calls(), 1);
		assert.strictEqual(state.stats.length, 1);

		state.settleStat();
		await flush();
		assert.strictEqual(tracker.settled(), false, "preparation should wait for the rules resolve");
		rulesGate.release(SETUP());
		assert.deepStrictEqual(await prepare, SETUP());
	});

	test("chmods the server binary when stat reports no exec bit", async () => {
		const rulesGate = deferred<RulesSetup>();
		const prepare = prepareClientStart(EXE, () => rulesGate.promise);

		await flush();
		assert.strictEqual(state.stats.length, 1);

		state.settleStat({ mode: 0o644 });
		await flush();
		assert.deepStrictEqual(state.chmods, [{ path: EXE, mode: 0o755 }]);

		rulesGate.release(SETUP());
		assert.deepStrictEqual(await prepare, SETUP());
	});

	test("leaves the binary alone when any exec bit is set", async () => {
		const prepare = prepareClientStart(EXE, () => Promise.resolve(SETUP()));

		await flush();
		state.settleStat({ mode: 0o744 });

		assert.deepStrictEqual(await prepare, SETUP());
		assert.deepStrictEqual(state.chmods, []);
		assert.deepStrictEqual(logger.logError.mock.calls, []);
	});

	test("logs and continues after a failed chmod", async () => {
		const err = new Error("EPERM: chmod went sideways");
		state.chmodError = err;
		const prepare = prepareClientStart(EXE, () => Promise.resolve(SETUP()));

		await flush();
		state.settleStat({ mode: 0o644 });

		assert.deepStrictEqual(
			await prepare,
			SETUP(),
			"activation should continue past a failed chmod",
		);
		assert.deepStrictEqual(state.chmods, [{ path: EXE, mode: 0o755 }]);
		assert.deepStrictEqual(logger.logError.mock.calls, [
			["stat/chmod error on server binary", err],
		]);
	});

	test("logs and continues after a failed binary fix-up", async () => {
		const err = new Error("EACCES: stat went sideways");
		const prepare = prepareClientStart(EXE, () => Promise.resolve(SETUP()));

		await flush();
		assert.strictEqual(state.stats.length, 1);

		state.settleStat(undefined, err);
		assert.deepStrictEqual(
			await prepare,
			SETUP(),
			"activation should continue past a failed stat/chmod",
		);
		assert.deepStrictEqual(logger.logError.mock.calls, [
			["stat/chmod error on server binary", err],
		]);
		assert.strictEqual(state.chmods.length, 0);
	});

	test("rejects with the rules-cache error without waiting for the binary fix-up", async () => {
		const err = new Error("cache dir went away");
		const rulesGate = deferred<RulesSetup>();
		const failingRules = (): Promise<RulesSetup> =>
			rulesGate.promise.then(() => {
				throw err;
			});

		const prepare = prepareClientStart(EXE, failingRules);
		await flush();
		assert.strictEqual(
			state.stats.length,
			1,
			"the binary fix-up should have started before the rules rejection",
		);

		// The stat is still pending here, so the rejection did not wait for it.
		rulesGate.release(SETUP());
		await assert.rejects(prepare, (e: unknown) => e === err);

		state.settleStat();
		await flush();
		assert.deepStrictEqual(logger.logError.mock.calls, []);
	});

	test("runs no binary fix-up on Windows", async () => {
		state.platform = "win32";
		const prepare = prepareClientStart(EXE, () => Promise.resolve(SETUP()));

		assert.deepStrictEqual(await prepare, SETUP());
		assert.deepStrictEqual(state.stats, [], "Windows has no exec bit to fix");
		assert.deepStrictEqual(state.chmods, []);
		assert.deepStrictEqual(logger.logError.mock.calls, []);
	});
});