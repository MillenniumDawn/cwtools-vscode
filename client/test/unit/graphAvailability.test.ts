import { suite, test } from "vitest";
import * as assert from "assert";
import { GRAPH_DATA_COMMAND, graphDataAvailable } from "../../extension/graphAvailability";

suite("graphAvailability", () => {
	test("available when the server advertises the command", () => {
		assert.strictEqual(graphDataAvailable(["getFileTypes", GRAPH_DATA_COMMAND]), true);
	});

	test("unavailable when the server advertises other commands", () => {
		assert.strictEqual(graphDataAvailable(["getFileTypes", "clearAllCaches"]), false);
	});

	test("unavailable when the server advertises nothing", () => {
		assert.strictEqual(graphDataAvailable([]), false);
	});

	// executeCommandProvider is optional in the LSP spec, so an older or
	// minimal server hands back undefined rather than an empty list.
	test("unavailable when the capability is absent", () => {
		assert.strictEqual(graphDataAvailable(undefined), false);
	});
});
