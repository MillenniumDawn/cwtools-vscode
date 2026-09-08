import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

const repoRoot = path.resolve(__dirname, "../../..");
const manifest = JSON.parse(
	fs.readFileSync(
		path.join(repoRoot, "extension", "package", "package.json"),
		"utf8",
	),
) as { activationEvents: string[] };

suite("manifest activation events", () => {
	test("only a root descriptor triggers workspace activation", () => {
		assert.deepStrictEqual(manifest.activationEvents, [
			"workspaceContains:descriptor.mod",
			"onWebviewPanel:cwtools-graph",
		]);
	});
});
