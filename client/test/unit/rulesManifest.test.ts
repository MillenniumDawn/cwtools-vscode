import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";
import { LANGUAGE_REPOS, RULES_MANIFEST_REVISION } from "../../extension/games";
import {
	parseRulesManifest,
	parseRulesManifestText,
	readRulesManifestBody,
	RULES_MANIFEST_MAX_BYTES,
	rulesRepoForManifest,
	selectRulesManifest,
} from "../../extension/rulesManifest";

const repoRoot = path.resolve(__dirname, "../../..");
const pins = Object.fromEntries(
	Object.entries(LANGUAGE_REPOS).map(([id, rules]) => [id, rules.ref]),
);

function manifest(
	overrides: Record<string, unknown> = {},
): Record<string, unknown> {
	return {
		schema: 1,
		revision: RULES_MANIFEST_REVISION,
		pins: { ...pins },
		...overrides,
	};
}

suite("rules manifest", () => {
	test("the checked-in manifest matches the bundled bootstrap pins", () => {
		const raw = fs.readFileSync(path.join(repoRoot, "rules-pins.json"), "utf8");
		const checkedIn = parseRulesManifestText(raw);
		assert.strictEqual(checkedIn.revision, RULES_MANIFEST_REVISION);
		assert.deepStrictEqual(checkedIn.pins, pins);
	});

	test("accepts a complete versioned pin set", () => {
		const parsed = parseRulesManifest(manifest());
		assert.strictEqual(parsed.schema, 1);
		assert.strictEqual(parsed.revision, RULES_MANIFEST_REVISION);
		assert.deepStrictEqual(parsed.pins, pins);
	});

	test("rejects malformed, stale, or repo-changing manifests", () => {
		const cases: Array<[string, Record<string, unknown>]> = [
			["stale revision", manifest({ revision: RULES_MANIFEST_REVISION - 1 })],
			[
				"missing game",
				(() => {
					const nextPins = { ...pins };
					delete nextPins.hoi4;
					return manifest({ pins: nextPins });
				})(),
			],
			[
				"unknown game",
				manifest({ pins: { ...pins, newgame: "f".repeat(40) } }),
			],
			["invalid ref", manifest({ pins: { ...pins, hoi4: "F".repeat(40) } })],
			["repo field", manifest({ repo: "https://example.invalid/rules" })],
		];
		for (const [name, value] of cases) {
			assert.throws(() => parseRulesManifest(value), /./, name);
		}
	});

	test("rejects malformed and oversized manifest text", () => {
		assert.throws(() => parseRulesManifestText("{"), /valid JSON/);
		assert.throws(
			() => parseRulesManifestText(" ".repeat(RULES_MANIFEST_MAX_BYTES + 1)),
			/too large/,
		);
	});

	test("bounds manifest response bodies before buffering them", async () => {
		const encoder = new TextEncoder();
		const body = new ReadableStream<Uint8Array>({
			start(controller) {
				controller.enqueue(encoder.encode('{"schema":'));
				controller.enqueue(encoder.encode("1}"));
				controller.close();
			},
		});
		assert.strictEqual(await readRulesManifestBody(body), '{"schema":1}');

		let cancelled = false;
		const oversized = new ReadableStream<Uint8Array>({
			start(controller) {
				controller.enqueue(new Uint8Array(RULES_MANIFEST_MAX_BYTES + 1));
			},
			cancel() {
				cancelled = true;
			},
		});
		await assert.rejects(
			() => readRulesManifestBody(oversized),
			/response is too large/,
		);
		assert.strictEqual(cancelled, true);
	});

	test("keeps a newer cached manifest and accepts a later reviewed revision", () => {
		const cached = parseRulesManifest(
			manifest({ revision: RULES_MANIFEST_REVISION + 2 }),
		);
		const stale = parseRulesManifest(
			manifest({ revision: RULES_MANIFEST_REVISION + 1 }),
		);
		const newer = parseRulesManifest(
			manifest({ revision: RULES_MANIFEST_REVISION + 3 }),
		);
		assert.strictEqual(selectRulesManifest(cached, stale), cached);
		assert.strictEqual(selectRulesManifest(cached, newer), newer);
	});

	test("rejects conflicting pins at the same revision", () => {
		const cached = parseRulesManifest(
			manifest({ revision: RULES_MANIFEST_REVISION + 1 }),
		);
		const conflicting = parseRulesManifest(
			manifest({
				revision: RULES_MANIFEST_REVISION + 1,
				pins: { ...pins, hoi4: "f".repeat(40) },
			}),
		);
		assert.throws(
			() => selectRulesManifest(cached, conflicting),
			/conflicts with the cached pins/,
		);
	});

	test("only replaces the ref of a bundled repository", () => {
		const reviewed = parseRulesManifest(
			manifest({
				revision: RULES_MANIFEST_REVISION + 1,
				pins: { ...pins, hoi4: "f".repeat(40) },
			}),
		);
		const rules = rulesRepoForManifest("hoi4", reviewed)!;
		assert.strictEqual(rules.repo, LANGUAGE_REPOS.hoi4.repo);
		assert.strictEqual(rules.ref, "f".repeat(40));
		assert.strictEqual(rulesRepoForManifest("unknown", reviewed), undefined);
	});
});
