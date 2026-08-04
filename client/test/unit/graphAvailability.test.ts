import { suite, test } from "vitest";
import * as assert from "assert";
import {
	GRAPH_DATA_COMMAND,
	graphDataAvailable,
	FIX_ALL_WORKSPACE_COMMAND,
	fixAllWorkspaceAvailable,
} from "../../extension/graphAvailability";

suite("graphAvailability", () => {
	test("available when the server advertises the command", () => {
		assert.strictEqual(
			graphDataAvailable(["getFileTypes", GRAPH_DATA_COMMAND]),
			true,
		);
	});

	test("unavailable when the server advertises other commands", () => {
		assert.strictEqual(
			graphDataAvailable(["getFileTypes", "clearAllCaches"]),
			false,
		);
	});

	test("unavailable when the server advertises nothing", () => {
		assert.strictEqual(graphDataAvailable([]), false);
	});

	// executeCommandProvider is optional in the LSP spec, so an older or
	// minimal server hands back undefined rather than an empty list.
	test("unavailable when the capability is absent", () => {
		assert.strictEqual(graphDataAvailable(undefined), false);
	});

	test("fixAllWorkspace available when advertised", () => {
		assert.strictEqual(
			fixAllWorkspaceAvailable(["getFileTypes", FIX_ALL_WORKSPACE_COMMAND]),
			true,
		);
	});

	test("fixAllWorkspace unavailable otherwise", () => {
		assert.strictEqual(
			fixAllWorkspaceAvailable(["getFileTypes", "clearAllCaches"]),
			false,
		);
		assert.strictEqual(fixAllWorkspaceAvailable([]), false);
		assert.strictEqual(fixAllWorkspaceAvailable(undefined), false);
	});
});
