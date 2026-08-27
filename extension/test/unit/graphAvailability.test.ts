import { suite, test } from "vitest";
import * as assert from "assert";
import {
	GRAPH_DATA_COMMAND,
	graphDataAvailable,
	FIX_ALL_WORKSPACE_COMMAND,
	fixAllWorkspaceAvailable,
	FORMAT_WORKSPACE_COMMAND,
	formatWorkspaceAvailable,
} from "../../src/host/graphAvailability";

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

	test("fixAllWorkspace available when advertised", () => {
		assert.strictEqual(fixAllWorkspaceAvailable(["getFileTypes", FIX_ALL_WORKSPACE_COMMAND]), true);
	});

	test("fixAllWorkspace unavailable otherwise", () => {
		assert.strictEqual(fixAllWorkspaceAvailable(["getFileTypes", "clearAllCaches"]), false);
		assert.strictEqual(fixAllWorkspaceAvailable([]), false);
		assert.strictEqual(fixAllWorkspaceAvailable(undefined), false);
	});

	test("formatWorkspace available when advertised", () => {
		assert.strictEqual(
			formatWorkspaceAvailable(["getFileTypes", FORMAT_WORKSPACE_COMMAND]),
			true,
		);
	});

	test("formatWorkspace unavailable otherwise", () => {
		assert.strictEqual(formatWorkspaceAvailable(["getFileTypes", "clearAllCaches"]), false);
		assert.strictEqual(formatWorkspaceAvailable([]), false);
		assert.strictEqual(formatWorkspaceAvailable(undefined), false);
	});
});
