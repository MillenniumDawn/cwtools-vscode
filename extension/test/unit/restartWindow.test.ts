import * as assert from "assert";
import { suite, test } from "vitest";
import { MAX_RESTARTS, recordCrash } from "../../src/host/restartWindow";

const SECOND = 1000;
const MINUTE = 60 * SECOND;

// Replays crashes at the given times through one history and returns, per
// crash, whether the server would be restarted.
function replay(times: number[]): boolean[] {
	const crashes: number[] = [];
	return times.map((t) => recordCrash(crashes, t));
}

const crashesEvery = (gapMs: number, count: number, start = 0): number[] =>
	Array.from({ length: count }, (_, i) => start + i * gapMs);

suite("restartWindow", () => {
	test("restarts the first four crashes and gives up on the fifth inside the window", () => {
		assert.strictEqual(MAX_RESTARTS, 4);
		assert.deepStrictEqual(replay(crashesEvery(10 * SECOND, 5)), [true, true, true, true, false]);
	});

	test("the window is 3 minutes, inclusive", () => {
		assert.strictEqual(replay(crashesEvery(45 * SECOND, 5))[4], false);
		assert.strictEqual(replay(crashesEvery(45 * SECOND + 1, 5))[4], true);
	});

	test("crashes spread past the window keep restarting", () => {
		assert.deepStrictEqual(replay(crashesEvery(61 * SECOND, 30)), Array<boolean>(30).fill(true));
	});

	test("a dense burst after a long quiet stretch still hits the limit", () => {
		const spread = crashesEvery(61 * SECOND, 10);
		const burst = crashesEvery(SECOND, 5, spread[spread.length - 1] + 10 * MINUTE);
		assert.deepStrictEqual(replay([...spread, ...burst]).slice(spread.length), [
			true,
			true,
			true,
			true,
			false,
		]);
	});
});
