import { suite, test } from "vitest";
import * as assert from "assert";
import { deriveNodeLabel } from "../../src/webview/graphLabel";

suite("graph labels", () => {
	test("handles malformed entity types without throwing", () => {
		assert.strictEqual(deriveNodeLabel("", undefined), "?");
		assert.strictEqual(deriveNodeLabel("_foo", undefined), "F");
		assert.strictEqual(deriveNodeLabel("a__b", undefined), "AB");
		assert.strictEqual(deriveNodeLabel(undefined, undefined), "?");
		assert.strictEqual(deriveNodeLabel(), "?");
	});
});
