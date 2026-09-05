import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

const manifest = JSON.parse(
	fs.readFileSync(
		path.resolve(__dirname, "../../package/package.json"),
		"utf8",
	),
) as {
	contributes: {
		configuration: Array<{
			properties: Record<string, { scope?: string }>;
		}>;
	};
};

function configurationScope(setting: string): string | undefined {
	for (const contribution of manifest.contributes.configuration) {
		const property = contribution.properties[setting];
		if (property) return property.scope;
	}
	return undefined;
}

suite("manifest — configuration scopes", () => {
	test("rules_folder is application-scoped and cannot be set per workspace", () => {
		assert.strictEqual(
			configurationScope("cwtools.rules_folder"),
			"application",
		);
	});
});
