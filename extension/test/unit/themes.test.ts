import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

// The shipped color themes are hand-authored JSON. These checks guard the two
// things that silently rot: the picker labels (must read "Paradox - <name>")
// and token coverage. Every scope the bundled TextMate grammars emit must be
// painted by every registered theme, or that token renders as plain text.
//
// Coverage is anchored on the two grammars the LSP server actually serves:
// paradox (game scripts, `source.mod`) and cwt (rule files, `source.cwt`). The
// per-game `stellaris`/`hoi4`/`eu4`/`ck2` grammars are intermediate and on the
// way out, so their extra scopes are not required here.

const packageDir = path.resolve(__dirname, "../../package");
const LSP_GRAMMARS = ["paradox.tmLanguage.json", "cwt.tmLanguage.json"];

// Container scopes wrap other tokens; painting them would tint whole blocks, so
// themes intentionally leave them alone. Root scope names never carry a token.
const NOT_REQUIRED = new Set([
	"source.mod",
	"source.cwt",
	"meta.block.paradox",
	"meta.construct.cwt",
	"meta.type-reference.cwt",
]);
const SEMANTIC_TOKEN_SELECTORS = [
	"type",
	"type.declaration",
	"enumMember",
	"variable",
	"namespace",
	"function",
];

interface Theme {
	name: string;
	type?: "dark" | "light";
	tokenColors?: Array<{ scope?: string | string[] }>;
	semanticHighlighting?: boolean;
	semanticTokenColors?: Record<string, { bold?: boolean }>;
	minimal?: boolean;
}

const readJson = (relative: string): unknown =>
	JSON.parse(fs.readFileSync(path.join(packageDir, relative), "utf8"));

const manifest = readJson("package.json") as {
	contributes: {
		themes: { label: string; path: string }[];
		configurationDefaults: Record<string, Record<string, unknown>>;
	};
};
const themes = manifest.contributes.themes.map((entry) => ({
	...entry,
	theme: readJson(entry.path) as Theme,
}));

// Every `name`/`contentName` scope the paradox and cwt grammars assign a token.
function grammarScopes(): string[] {
	const found = new Set<string>();
	const collect = (node: unknown): void => {
		if (Array.isArray(node)) {
			node.forEach(collect);
		} else if (node && typeof node === "object") {
			for (const [key, value] of Object.entries(node)) {
				if (
					(key === "name" || key === "contentName") &&
					typeof value === "string" &&
					value.includes(".")
				) {
					found.add(value);
				} else {
					collect(value);
				}
			}
		}
	};
	for (const file of LSP_GRAMMARS) collect(readJson(`syntaxes/${file}`));
	return [...found].filter((scope) => !NOT_REQUIRED.has(scope)).sort();
}

function paintedScopes(theme: Theme): string[] {
	return (theme.tokenColors ?? []).flatMap((rule) =>
		(Array.isArray(rule.scope) ? rule.scope : (rule.scope ?? "").split(","))
			.map((scope) => scope.trim())
			.filter(Boolean),
	);
}

suite("themes", () => {
	test("themes are registered once each, with a dark and a light variant", () => {
		const unique = (values: string[]) => new Set(values).size === values.length;
		assert.ok(unique(themes.map((t) => t.label)), "duplicate theme labels");
		assert.ok(unique(themes.map((t) => t.path)), "duplicate theme paths");
		const types = themes.map((t) => t.theme.type);
		assert.ok(types.includes("dark") && types.includes("light"), types.join());
	});

	test.each(themes)("$label is labelled, typed and paints semantic tokens", ({ label, theme }) => {
		assert.match(label, /^Paradox - \S/);
		assert.strictEqual(theme.name, label);
		assert.ok(theme.type === "dark" || theme.type === "light");
		assert.strictEqual(theme.semanticHighlighting, true);
		assert.deepStrictEqual(
			SEMANTIC_TOKEN_SELECTORS.filter(
				(selector) => !(selector in (theme.semanticTokenColors ?? {})),
			),
			[],
		);
		assert.deepStrictEqual(theme.semanticTokenColors?.["type.declaration"], {
			bold: true,
		});
	});

	test("semantic highlighting defaults on only for game scripts", () => {
		const defaults = manifest.contributes.configurationDefaults;
		const key = "editor.semanticHighlighting.enabled";
		assert.strictEqual(defaults["[paradox]"][key], true);
		assert.strictEqual(defaults["[cwt]"]?.[key], undefined);
	});

	const required = grammarScopes();

	test("grammars emit a non-trivial scope set", () => {
		assert.ok(required.length > 40, `only ${required.length} required scopes`);
	});

	// A theme covers a scope when a rule targets it exactly or targets a less
	// specific prefix (TextMate scope-selector matching).
	test.each(themes)("$label paints every grammar scope exactly once", ({ theme }) => {
		const painted = paintedScopes(theme);
		assert.deepStrictEqual(
			painted.filter((scope, index) => painted.indexOf(scope) !== index),
			[],
			"duplicate scope rules",
		);
		// Minimal themes defer colour to VS Code, so coverage doesn't apply.
		if (theme.minimal) return;
		assert.deepStrictEqual(
			required.filter(
				(scope) =>
					!painted.some((rule) => rule === scope || scope.startsWith(rule + ".")),
			),
			[],
			"unstyled scopes",
		);
	});
});
