import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

// The paradox TextMate grammar is hand-edited JSON, so these assert on the raw
// regex strings it ships, compiled as JS RegExps.

const packageDir = path.resolve(__dirname, "../../package");
const readJson = (relative: string): unknown =>
	JSON.parse(fs.readFileSync(path.join(packageDir, relative), "utf8"));

interface Pattern {
	name?: string;
	match?: string;
}
const grammar = readJson("syntaxes/paradox.tmLanguage.json") as {
	repository: {
		keywords: { patterns: Pattern[] };
		strings: { end: string };
		block: { patterns: Array<{ begin: string }> };
	};
};

// Find the `match` regex of the grammar rule with the given scope name.
function ruleMatch(name: string): string {
	let match: string | undefined;
	const walk = (node: unknown): void => {
		if (Array.isArray(node)) {
			node.forEach(walk);
		} else if (node && typeof node === "object") {
			const obj = node as Pattern;
			if (obj.name === name && typeof obj.match === "string") {
				match = obj.match;
			}
			Object.values(obj).forEach(walk);
		}
	};
	walk(grammar);
	assert.ok(match, `no match regex for rule: ${name}`);
	return match;
}

// Identifiers with digits (my_focus_2b, my_event.2b, 2b_tag) used to be
// mis-tokenized: the #id rule's unescaped dot and missing trailing anchor
// mangled plain ids, and the generic digit-run rule split digit-first tags.
suite("paradox grammar — identifier tokenization (#73)", () => {
	const id = new RegExp(ruleMatch("meta.id.paradox"));
	const rhs = ruleMatch("constant.rhs.paradox");

	test("the #id rule splits a real event id and leaves digit-suffixed ids alone", () => {
		assert.strictEqual("id = civil_war.1".match(id)?.[5], "1");
		assert.strictEqual(id.test("id = my_focus_2b"), false);
		assert.strictEqual(id.test("id = my_event.2b"), false);
	});

	test("constant.rhs.paradox matches a full date but not a digit-first identifier", () => {
		assert.strictEqual("1936.1.1".match(new RegExp(rhs))?.[0], "1936.1.1");
		assert.strictEqual(new RegExp("^(?:" + rhs + ")").test("2b_tag"), false);
	});
});

// The merged keyword table had 465 words in two or three color buckets, so
// which color won was an accident of pattern order. The invariant is one word,
// one bucket.
suite("paradox grammar — keyword word lists (#112)", () => {
	const keywordPatterns = grammar.repository.keywords.patterns;
	const wordList = /^\\b\((\w+(?:\|\w+)*)\)\\b$/;
	const buckets = new Map(
		keywordPatterns.flatMap((p): [string, string[]][] => {
			const words = p.match?.match(wordList)?.[1];
			return words ? [[p.name ?? "?", words.split("|")]] : [];
		}),
	);
	const bucket = (name: string): string[] => {
		const words = buckets.get(name);
		assert.ok(words, `missing bucket: ${name}`);
		return words;
	};

	test("no two patterns share a scope name and no word appears in two buckets", () => {
		const names = keywordPatterns.map((p) => p.name);
		assert.strictEqual(new Set(names).size, names.length, names.join(", "));

		const owners = new Map<string, string[]>();
		for (const [name, words] of buckets) {
			for (const word of words) {
				owners.set(word, [...(owners.get(word) ?? []), name]);
			}
		}
		assert.deepStrictEqual(
			[...owners].filter(([, names]) => names.length > 1),
			[],
		);
	});

	test("every word list is sorted and keeps yes/no as RHS constants", () => {
		assert.ok(buckets.size >= 10, `only ${buckets.size} word lists`);
		for (const [name, words] of buckets) {
			const sorted = [...words].sort((a, b) => {
				const [la, lb] = [a.toLowerCase(), b.toLowerCase()];
				return la < lb ? -1 : la > lb ? 1 : a < b ? -1 : a > b ? 1 : 0;
			});
			assert.deepStrictEqual(words, sorted, `${name} is not sorted`);
			assert.ok(!words.includes("yes") && !words.includes("no"), name);
		}
	});

	test("iterators sit in their scope buckets", () => {
		const stray = (name: string, prefix: RegExp) =>
			bucket(name).filter((word) => prefix.test(word) && word !== "random_list");
		assert.deepStrictEqual(
			stray("variable.language.condition_scopes.paradox", /^(every|random)_/),
			[],
		);
		assert.deepStrictEqual(
			stray("variable.language.command_scopes.paradox", /^(any|count)_/),
			[],
		);
		for (const name of [
			"variable.language.conditions.paradox",
			"variable.language.effects.paradox",
		]) {
			assert.deepStrictEqual(stray(name, /^(any|count|every|random)_/), [], name);
		}
	});
});

// An unterminated string used to flood every following line, and a `{` the old
// lookbehind couldn't open still had its `}` close the enclosing block,
// shifting nesting for the rest of the file.
suite("paradox grammar — strings and block (#112)", () => {
	test("a string terminates at end of line", () => {
		assert.strictEqual(grammar.repository.strings.end, '"|$');
	});

	test("block begin opens braces the old lookbehind missed", () => {
		const begin = new RegExp(grammar.repository.block.patterns[0].begin);
		assert.strictEqual("{".match(begin)?.index, 0);
		assert.strictEqual("rgb {".match(begin)?.index, 4);
		// Must start right after the '=' to tie (and beat, by rule order) the
		// RHS-value rule in #variables; one position later loses the race.
		assert.strictEqual("a = {".match(begin)?.index, 3);
		assert.strictEqual("word{".match(begin), null);
	});

	test("long generated lines stop tokenizing at the guard", () => {
		const manifest = readJson("package.json") as {
			contributes: {
				configurationDefaults: Record<string, Record<string, number>>;
			};
		};
		assert.strictEqual(
			manifest.contributes.configurationDefaults["[paradox]"][
				"editor.maxTokenizationLineLength"
			],
			10000,
		);
	});
});
