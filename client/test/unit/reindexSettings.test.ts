import { suite, test } from "vitest";
import * as assert from "assert";
import {
	normalizeBackgroundReindexMinutes,
	normalizeBackgroundReindexIdleSeconds,
	buildSettingsPayload,
	mapIgnoreOptions,
} from "../../extension/reindexSettings";

suite("reindexSettings — normalizeBackgroundReindexMinutes", () => {
	test("defaults to 30 when the setting is unset", () => {
		assert.strictEqual(normalizeBackgroundReindexMinutes(undefined), 30);
	});

	test("preserves an explicit 0 (disabled)", () => {
		assert.strictEqual(normalizeBackgroundReindexMinutes(0), 0);
	});

	test("passes a normal value through", () => {
		assert.strictEqual(normalizeBackgroundReindexMinutes(45), 45);
	});
});

suite("reindexSettings — normalizeBackgroundReindexIdleSeconds", () => {
	test("defaults to the server's 15s when the setting is unset", () => {
		assert.strictEqual(normalizeBackgroundReindexIdleSeconds(undefined), 15);
	});

	test("passes a normal value through", () => {
		assert.strictEqual(normalizeBackgroundReindexIdleSeconds(60), 60);
	});

	test("preserves an explicit 0 (no idle wait)", () => {
		assert.strictEqual(normalizeBackgroundReindexIdleSeconds(0), 0);
	});
});

suite("reindexSettings — mapIgnoreOptions", () => {
	test("maps bare file names to **/ globs", () => {
		const result = mapIgnoreOptions([], ["README.txt", "credits.txt"], []);
		assert.deepStrictEqual(result.ignoreFilePatterns, [
			"**/README.txt",
			"**/credits.txt",
		]);
	});

	test("rewrites a slashless glob to match anywhere (*.txt becomes **/*.txt)", () => {
		const result = mapIgnoreOptions([], ["*.txt"], []);
		assert.deepStrictEqual(result.ignoreFilePatterns, ["**/*.txt"]);
	});

	test("passes names that already contain a path through unchanged", () => {
		const result = mapIgnoreOptions([], ["docs/README.txt"], []);
		assert.deepStrictEqual(result.ignoreFilePatterns, ["docs/README.txt"]);
	});

	test("empty or missing settings produce empty lists", () => {
		assert.deepStrictEqual(mapIgnoreOptions(undefined, undefined, undefined), {
			ignoreFilePatterns: [],
			ignoredErrorCodes: [],
		});
		assert.deepStrictEqual(mapIgnoreOptions([], [], []), {
			ignoreFilePatterns: [],
			ignoredErrorCodes: [],
		});
	});

	test("merges ignore_patterns globs ahead of the mapped file names", () => {
		const result = mapIgnoreOptions(
			["**/99_README**.txt"],
			["credits.txt"],
			[],
		);
		assert.deepStrictEqual(result.ignoreFilePatterns, [
			"**/99_README**.txt",
			"**/credits.txt",
		]);
	});

	test("passes ignored error codes through", () => {
		const result = mapIgnoreOptions([], [], ["CW100", "CW222"]);
		assert.deepStrictEqual(result.ignoredErrorCodes, ["CW100", "CW222"]);
	});
});

suite("reindexSettings — buildSettingsPayload", () => {
	const liveSettings = {
		localisationLanguages: ["English"],
		hoverShowAllLanguages: false,
		hoverDebug: false,
		hoverScopeDisplay: "context",
	};

	test("carries the server keys and spreads the ignore options", () => {
		const ignore = { ignoreFilePatterns: ["**/x.txt"], ignoredErrorCodes: ["CW100"] };
		const payload = buildSettingsPayload(ignore, 10, 45, liveSettings);
		assert.strictEqual(payload.backgroundReindexIntervalMinutes, 10);
		assert.strictEqual(payload.backgroundReindexIdleSeconds, 45);
		assert.deepStrictEqual(payload.ignoreFilePatterns, ["**/x.txt"]);
		assert.deepStrictEqual(payload.ignoredErrorCodes, ["CW100"]);
		assert.deepStrictEqual(payload.localisationLanguages, ["English"]);
		assert.strictEqual(payload.hoverShowAllLanguages, false);
		assert.strictEqual(payload.hoverDebug, false);
		assert.strictEqual(payload.hoverScopeDisplay, "context");
	});

	test("defaults the interval and idle window when unset in the payload too", () => {
		const payload = buildSettingsPayload({}, undefined, undefined, liveSettings);
		assert.strictEqual(payload.backgroundReindexIntervalMinutes, 30);
		assert.strictEqual(payload.backgroundReindexIdleSeconds, 15);
	});
});
