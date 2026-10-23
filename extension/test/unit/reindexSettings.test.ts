import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";
import {
	normalizeBackgroundReindexMinutes,
	normalizeBackgroundReindexIdleSeconds,
	buildSettingsPayload,
	mapIgnoreOptions,
	isLiveSettingsChange,
	isReloadSettingsChange,
	LIVE_SETTINGS_KEYS,
	RELOAD_SETTINGS_KEYS,
} from "../../src/host/reindexSettings";
import type { LiveServerSettings } from "../../src/host/reindexSettings";

suite("reindexSettings — normalization", () => {
	test("an unset interval or idle window takes the server default, an explicit 0 survives", () => {
		assert.strictEqual(normalizeBackgroundReindexMinutes(undefined), 30);
		assert.strictEqual(normalizeBackgroundReindexMinutes(0), 0);
		assert.strictEqual(normalizeBackgroundReindexMinutes(45), 45);
		assert.strictEqual(normalizeBackgroundReindexIdleSeconds(undefined), 15);
		assert.strictEqual(normalizeBackgroundReindexIdleSeconds(0), 0);
		assert.strictEqual(normalizeBackgroundReindexIdleSeconds(60), 60);
	});

	test("ignore_patterns globs come ahead of file names, which pass through unprefixed", () => {
		assert.deepStrictEqual(
			mapIgnoreOptions(
				["**/99_README**.txt"],
				["credits.txt", "*.txt", "docs/README.txt"],
				["CW100", "CW222"],
			),
			{
				ignoreFilePatterns: [
					"**/99_README**.txt",
					"credits.txt",
					"*.txt",
					"docs/README.txt",
				],
				ignoredErrorCodes: ["CW100", "CW222"],
			},
		);
	});

	test("missing ignore settings produce empty lists", () => {
		assert.deepStrictEqual(mapIgnoreOptions(undefined, undefined, undefined), {
			ignoreFilePatterns: [],
			ignoredErrorCodes: [],
		});
	});
});

suite("reindexSettings — buildSettingsPayload", () => {
	const liveSettings: LiveServerSettings = {
		diagnosticsBaseline: ".cwtools-baseline",
		localisationLanguages: ["English", "French"],
		workspaceWideDiagnostics: true,
		hoverShowAllLanguages: false,
		hoverDebug: false,
		hoverScopeDisplay: "resolved",
		formattingIndentStyle: "tab",
		formattingIndentSize: 2,
		formattingMaxLineWidth: 80,
		formattingTrimTrailingWhitespace: false,
		formattingInsertFinalNewline: true,
	};

	test("carries the server keys, the ignore options and every live setting", () => {
		const ignore = {
			ignoreFilePatterns: ["**/x.txt"],
			ignoredErrorCodes: ["CW100"],
		};
		assert.deepStrictEqual(buildSettingsPayload(ignore, 10, 45, liveSettings), {
			...ignore,
			...liveSettings,
			backgroundReindexIntervalMinutes: 10,
			backgroundReindexIdleSeconds: 45,
		});
	});

	// The server reads an empty language list as "no language", so it must not
	// be dropped or defaulted on the way out.
	test("defaults the interval and idle window and preserves an empty language list", () => {
		const payload = buildSettingsPayload({}, undefined, undefined, {
			...liveSettings,
			localisationLanguages: [],
		});
		assert.strictEqual(payload.backgroundReindexIntervalMinutes, 30);
		assert.strictEqual(payload.backgroundReindexIdleSeconds, 15);
		assert.deepStrictEqual(payload.localisationLanguages, []);
	});

	test("does not mutate the ignore options and overwrites a colliding key", () => {
		const ignore = {
			ignoreFilePatterns: ["**/x.txt"],
			backgroundReindexIntervalMinutes: 999,
		};
		const payload = buildSettingsPayload(ignore, 10, 20, liveSettings);
		assert.strictEqual(payload.backgroundReindexIntervalMinutes, 10);
		assert.strictEqual(ignore.backgroundReindexIntervalMinutes, 999);
	});
});

suite("reindexSettings — settings change classification", () => {
	const touching = (...touched: string[]) => ({
		affectsConfiguration: (section: string) => touched.includes(section),
	});

	test("classifies every manifest cwtools setting exactly once", () => {
		const manifest = JSON.parse(
			fs.readFileSync(
				path.resolve(__dirname, "../../package/package.json"),
				"utf8",
			),
		) as {
			contributes: {
				configuration: Array<{ properties: Record<string, unknown> }>;
			};
		};
		const settings = manifest.contributes.configuration.flatMap((section) =>
			Object.keys(section.properties).filter((key) =>
				key.startsWith("cwtools."),
			),
		);
		const live: readonly string[] = LIVE_SETTINGS_KEYS;
		const reload: readonly string[] = RELOAD_SETTINGS_KEYS;
		assert.deepStrictEqual(
			settings.filter((key) => live.includes(key) === reload.includes(key)),
			[],
			"each setting must be live or reload, not both and not neither",
		);
	});

	test("each reload key asks for a window reload, live and unrelated keys do not", () => {
		for (const key of RELOAD_SETTINGS_KEYS) {
			assert.strictEqual(isReloadSettingsChange(touching(key)), true, key);
		}
		for (const key of ["cwtools.rules_folder", "cwtools.hover.debug", "cwtools.notASetting"]) {
			assert.strictEqual(isReloadSettingsChange(touching(key)), false, key);
		}
		assert.strictEqual(isReloadSettingsChange(touching()), false);
	});

	test("each live key applies live, including the rules settings, reload keys do not", () => {
		for (const key of [...LIVE_SETTINGS_KEYS, "cwtools.rules_folder", "cwtools.rules.autoReload"]) {
			assert.strictEqual(isLiveSettingsChange(touching(key)), true, key);
		}
		assert.strictEqual(
			isLiveSettingsChange(touching("cwtools.cache.hoi4", "cwtools.hover.debug")),
			true,
		);
		assert.strictEqual(isLiveSettingsChange(touching("cwtools.cache.hoi4")), false);
		assert.strictEqual(isLiveSettingsChange(touching()), false);
	});
});
