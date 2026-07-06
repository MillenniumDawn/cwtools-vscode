import { suite, test } from "vitest";
import * as assert from "assert";
import {
	normalizeBackgroundReindexMinutes,
	buildReindexSettingsPayload,
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

suite("reindexSettings — buildReindexSettingsPayload", () => {
	test("carries the interval under the server's key and spreads the ignore options", () => {
		const ignore = { ignoreFilePatterns: ["**/x.txt"], ignoredErrorCodes: ["CW100"] };
		const payload = buildReindexSettingsPayload(ignore, 10);
		assert.strictEqual(payload.backgroundReindexIntervalMinutes, 10);
		assert.deepStrictEqual(payload.ignoreFilePatterns, ["**/x.txt"]);
		assert.deepStrictEqual(payload.ignoredErrorCodes, ["CW100"]);
	});

	test("defaults the interval when unset in the payload too", () => {
		const payload = buildReindexSettingsPayload({}, undefined);
		assert.strictEqual(payload.backgroundReindexIntervalMinutes, 30);
	});
});
