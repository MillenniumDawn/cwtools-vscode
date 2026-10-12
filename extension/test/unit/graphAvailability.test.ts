import { suite, test } from "vitest";
import * as assert from "assert";
import {
	GRAPH_DATA_COMMAND,
	graphDataAvailable,
	FIX_ALL_WORKSPACE_COMMAND,
	fixAllWorkspaceAvailable,
	FORMAT_WORKSPACE_COMMAND,
	formatWorkspaceAvailable,
	VALIDATE_WORKSPACE_COMMAND,
	validateWorkspaceAvailable,
} from "../../src/host/graphAvailability";

suite("graphAvailability", () => {
	test.each([
		[GRAPH_DATA_COMMAND, graphDataAvailable],
		[FIX_ALL_WORKSPACE_COMMAND, fixAllWorkspaceAvailable],
		[FORMAT_WORKSPACE_COMMAND, formatWorkspaceAvailable],
		[VALIDATE_WORKSPACE_COMMAND, validateWorkspaceAvailable],
	])("%s is available only when the server advertises it", (command, available) => {
		assert.strictEqual(available(["getFileTypes", command]), true);
		assert.strictEqual(available(["getFileTypes", "clearAllCaches"]), false);
		assert.strictEqual(available([]), false);
		// executeCommandProvider is optional in the LSP spec, so an older or
		// minimal server hands back undefined rather than an empty list.
		assert.strictEqual(available(undefined), false);
	});
});
