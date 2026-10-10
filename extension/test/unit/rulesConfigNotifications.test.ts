import { suite, test } from "vitest";
import * as assert from "assert";
import { isRulesConfigSummaryNotification } from "../../src/common/rulesConfigNotifications";

suite("rulesConfigNotifications — isRulesConfigSummaryNotification", () => {
	test("matches the rules-config summary envelope", () => {
		assert.strictEqual(
			isRulesConfigSummaryNotification(
				"CWTools: 116 rules-config error(s), first: config/triggers.cwt:3090:0: CW605",
			),
			true,
		);
	});

	test("keeps command failures containing the summary phrase visible", () => {
		assert.strictEqual(
			isRulesConfigSummaryNotification(
				"CWTools: formatWorkspace failed: rules-config error(s) while loading",
			),
			false,
		);
	});
});
