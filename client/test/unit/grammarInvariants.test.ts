import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

// Guards the #112 grammar rework. The merged keyword table had 465 words in
// two or three color buckets, so which color won was an accident of pattern
// order ("theme disco"). The invariant is one word, one bucket. The strings
// and block rules are pinned too: an unterminated string used to flood every
// following line, and a `{` the old lookbehind couldn't open still had its
// `}` close the enclosing block, shifting nesting for the rest of the file.

const repoRoot = path.resolve(__dirname, "../../..");
const grammar = JSON.parse(
	fs.readFileSync(
		path.join(repoRoot, "release", "syntaxes", "paradox.tmLanguage.json"),
		"utf8",
	),
) as {
	repository: {
		keywords: { patterns: Pattern[] };
		strings: { end: string };
		block: { patterns: Array<{ begin: string }> };
	};
};
const manifest = JSON.parse(
	fs.readFileSync(path.join(repoRoot, "release", "package.json"), "utf8"),
) as {
	contributes: {
		configurationDefaults: Record<string, Record<string, number>>;
	};
};

interface Pattern {
	name?: string;
	match?: string;
}
const keywordPatterns: Pattern[] = grammar.repository.keywords.patterns;
const wordList = /^\\b\((\w+(?:\|\w+)*)\)\\b$/;
const wordListPatterns = keywordPatterns.filter(
	(p) => p.match !== undefined && wordList.test(p.match),
);

function wordsOf(p: Pattern): string[] {
	const m = p.match?.match(wordList);
	assert.ok(m, `not a word list: ${p.name}`);
	return m[1].split("|");
}

suite("paradox grammar — keyword word lists (#112)", () => {
	test("the expected buckets are all present as word lists", () => {
		const names = wordListPatterns.map((p) => p.name);
		for (const bucket of [
			"variable.boolean.paradox",
			"variable.language.multi_scopes.paradox",
			"variable.language.condition_scopes.paradox",
			"variable.language.command_scopes.paradox",
			"variable.language.effects.paradox",
			"variable.language.conditions.paradox",
			"variable.language.modifiers.paradox",
			"variable.language.keywords",
			"variable.language.other.paradox",
			"variable.language.definition_tokens.paradox",
		]) {
			assert.ok(names.includes(bucket), `missing bucket: ${bucket}`);
		}
	});

	test("no word appears in two buckets", () => {
		const owners = new Map<string, string[]>();
		for (const p of wordListPatterns) {
			for (const w of wordsOf(p)) {
				owners.set(w, [...(owners.get(w) ?? []), p.name ?? "?"]);
			}
		}
		const collisions = [...owners].filter(([, ns]) => ns.length > 1);
		assert.deepStrictEqual(
			collisions,
			[],
			`words with more than one color: ${collisions
				.slice(0, 10)
				.map(([w, ns]) => `${w} (${ns.join(", ")})`)
				.join("; ")}`,
		);
	});

	test("no two patterns share a scope name", () => {
		const names = keywordPatterns.map((p) => p.name);
		assert.strictEqual(new Set(names).size, names.length, names.join(", "));
	});

	test("every word list compiles, is sorted, and has no duplicates", () => {
		for (const p of wordListPatterns) {
			assert.doesNotThrow(() => new RegExp(p.match ?? ""));
			const words = wordsOf(p);
			assert.strictEqual(new Set(words).size, words.length, p.name ?? "?");
			const sorted = [...words].sort((a, b) => {
				const [la, lb] = [a.toLowerCase(), b.toLowerCase()];
				return la < lb ? -1 : la > lb ? 1 : a < b ? -1 : a > b ? 1 : 0;
			});
			assert.deepStrictEqual(words, sorted, `${p.name} is not sorted`);
		}
	});

	test("iterators sit in their scope buckets", () => {
		const byName = new Map(wordListPatterns.map((p) => [p.name, wordsOf(p)]));
		const conditionScopes =
			byName.get("variable.language.condition_scopes.paradox") ?? [];
		const commandScopes =
			byName.get("variable.language.command_scopes.paradox") ?? [];
		assert.deepStrictEqual(
			conditionScopes.filter((w) => /^(every|random)_/.test(w)),
			[],
		);
		assert.deepStrictEqual(
			commandScopes.filter((w) => /^(any|count)_/.test(w)),
			[],
		);
		for (const bucket of [
			"variable.language.conditions.paradox",
			"variable.language.effects.paradox",
		]) {
			assert.deepStrictEqual(
				(byName.get(bucket) ?? []).filter(
					(w) =>
						/^(any|count|every|random)_/.test(w) && w !== "random_list",
				),
				[],
				bucket,
			);
		}
	});

	test("yes/no stay RHS constants, not keys", () => {
		for (const p of wordListPatterns) {
			const words = wordsOf(p);
			assert.ok(!words.includes("yes") && !words.includes("no"), p.name ?? "?");
		}
	});
});

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
		assert.strictEqual(
			manifest.contributes.configurationDefaults["[paradox]"][
				"editor.maxTokenizationLineLength"
			],
			10000,
		);
	});
});
