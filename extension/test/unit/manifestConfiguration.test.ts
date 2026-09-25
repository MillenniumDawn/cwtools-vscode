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
			properties: Record<
				string,
				{ scope?: string; type?: string; default?: unknown }
			>;
		}>;
	};
};

function configurationProperty(
	setting: string,
): { scope?: string; type?: string; default?: unknown } | undefined {
	for (const contribution of manifest.contributes.configuration) {
		const property = contribution.properties[setting];
		if (property) return property;
	}
	return undefined;
}

function configurationScope(setting: string): string | undefined {
	return configurationProperty(setting)?.scope;
}

suite("manifest — configuration scopes", () => {
	test("rules_folder is application-scoped and cannot be set per workspace", () => {
		assert.strictEqual(
			configurationScope("cwtools.rules_folder"),
			"application",
		);
	});

	test("enable is a window-scoped boolean enabled by default", () => {
		assert.deepStrictEqual(configurationProperty("cwtools.enable"), {
			scope: "window",
			type: "boolean",
			default: true,
			description: "%configuration.cwtools.enable.description%",
		});
	});
});
