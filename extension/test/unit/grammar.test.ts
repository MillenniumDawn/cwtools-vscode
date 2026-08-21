import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

// Guards the #73 grammar fixes in paradox.tmLanguage.json. Identifiers with
// digits (my_focus_2b, my_event.2b, 2b_tag) used to be mis-tokenized: the #id
// rule's unescaped dot + missing trailing anchor mangled plain ids, and the
// generic digit-run rule split digit-first tags. We assert on the raw regex
// strings the grammar ships, compiled as JS RegExps.

const repoRoot = path.resolve(__dirname, "../../..");
const grammarPath = path.join(
	repoRoot,
	"extension",
	"package",
	"syntaxes",
	"paradox.tmLanguage.json",
);
const grammar = JSON.parse(fs.readFileSync(grammarPath, "utf8")) as unknown;

// Find the `match` regex of the grammar rule with the given scope name.
function ruleMatch(name: string): string {
	let match: string | undefined;
	const walk = (node: unknown): void => {
		if (Array.isArray(node)) {
			node.forEach(walk);
		} else if (node && typeof node === "object") {
			const obj = node as Record<string, unknown>;
			if (obj.name === name && typeof obj.match === "string") {
				match = obj.match;
			}
			for (const value of Object.values(obj)) walk(value);
		}
	};
	walk(grammar);
	assert.ok(match, `no match regex for rule: ${name}`);
	return match;
}

const idMatch = ruleMatch("meta.id.paradox");
const rhsMatch = ruleMatch("constant.rhs.paradox");

suite("paradox grammar — #73 identifier tokenization", () => {
	const idRegex = new RegExp(idMatch);

	test("#id rule leaves digit-suffixed ids alone", () => {
		assert.strictEqual(idRegex.test("id = my_focus_2b"), false);
		assert.strictEqual(idRegex.test("id = my_event.2b"), false);
	});

	test("#id rule still splits a real event id", () => {
		const m = "id = civil_war.1".match(idRegex);
		assert.ok(m, "expected civil_war.1 to match the #id rule");
		assert.strictEqual(m[5], "1");
	});

	test("constant.rhs.paradox does not fire on a digit-first identifier", () => {
		const anchored = new RegExp("^(?:" + rhsMatch + ")");
		assert.strictEqual(anchored.test("2b_tag"), false);
	});

	test("constant.rhs.paradox matches a full date", () => {
		const m = "1936.1.1".match(new RegExp(rhsMatch));
		assert.ok(m, "expected 1936.1.1 to match");
		assert.strictEqual(m[0], "1936.1.1");
	});
});
