import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

// The shipped color themes are hand-authored JSON. These checks guard the two
// things that silently rot: the picker labels (must read "Paradox - <name>")
// and token coverage — every scope the bundled TextMate grammars emit must be
// painted by every registered theme, or that token renders as plain text.
//
// Coverage is anchored on the two grammars the LSP server actually serves:
// paradox (game scripts, `source.mod`) and cwt (rule files, `source.cwt`). The
// game is detected at runtime from the mod, not from a per-language grammar;
// the per-game `stellaris`/`hoi4`/`eu4`/`ck2` grammars are intermediate and on
// the way out, so we do not require their extra scopes here.

const repoRoot = path.resolve(__dirname, "../../..");
const releaseDir = path.join(repoRoot, "release");
const syntaxesDir = path.join(releaseDir, "syntaxes");
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

interface ThemeEntry {
	label: string;
	path: string;
}

interface Theme {
	name: string;
	type?: "dark" | "light";
	tokenColors?: Array<{ scope?: string | string[] }>;
	semanticHighlighting?: boolean;
	semanticTokenColors?: Record<string, { bold?: boolean }>;
	minimal?: boolean;
}

function registeredThemes(): ThemeEntry[] {
	const manifest = JSON.parse(
		fs.readFileSync(path.join(releaseDir, "package.json"), "utf8"),
	) as { contributes: { themes: ThemeEntry[] } };
	return manifest.contributes.themes;
}

// Every `name`/`contentName` scope string the paradox + cwt grammars assign to
// a token.
function grammarScopes(): Set<string> {
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
	for (const file of LSP_GRAMMARS) {
		const full = path.join(syntaxesDir, file);
		assert.ok(fs.existsSync(full), `missing grammar: ${file}`);
		collect(JSON.parse(fs.readFileSync(full, "utf8")));
	}
	return found;
}

function themeScopes(theme: {
	tokenColors?: { scope?: string | string[] }[];
}): Set<string> {
	const out = new Set<string>();
	for (const rule of theme.tokenColors ?? []) {
		if (!rule.scope) continue;
		const list = Array.isArray(rule.scope) ? rule.scope : rule.scope.split(",");
		for (const raw of list) {
			const s = raw.trim();
			if (s) out.add(s);
		}
	}
	return out;
}

// A theme covers a scope when a rule targets it exactly or targets a less
// specific prefix (TextMate scope-selector matching).
function covers(rules: Set<string>, scope: string): boolean {
	for (const rule of rules) {
		if (rule === scope || scope.startsWith(rule + ".")) return true;
	}
	return false;
}

const required = [...grammarScopes()]
	.filter((s) => !NOT_REQUIRED.has(s))
	.sort();
const themes = registeredThemes();
const SEMANTIC_TOKEN_SELECTORS = [
	"type",
	"type.declaration",
	"enumMember",
	"variable",
	"namespace",
	"function",
];

suite("themes — registration", () => {
	test('every registered theme is labelled "Paradox - <name>"', () => {
		assert.ok(themes.length > 0, "no themes registered");
		for (const t of themes) {
			assert.match(t.label, /^Paradox - \S/, `bad label: ${t.label}`);
		}
	});

	test("every theme file exists, parses, and its name matches its label", () => {
		for (const t of themes) {
			const file = path.join(releaseDir, t.path);
			assert.ok(fs.existsSync(file), `missing theme file: ${t.path}`);
			const theme = JSON.parse(fs.readFileSync(file, "utf8")) as Theme;
			assert.strictEqual(
				theme.name,
				t.label,
				`name/label mismatch in ${t.path}`,
			);
			assert.ok(
				theme.type === "dark" || theme.type === "light",
				`bad type in ${t.path}`,
			);
			assert.ok(
				Array.isArray(theme.tokenColors),
				`no tokenColors in ${t.path}`,
			);
		}
	});

	test("every theme enables and paints semantic tokens", () => {
		for (const t of themes) {
			const theme = JSON.parse(
				fs.readFileSync(path.join(releaseDir, t.path), "utf8"),
			) as Theme;
			assert.strictEqual(
				theme.semanticHighlighting,
				true,
				`semantic highlighting is disabled in ${t.path}`,
			);
			assert.ok(
				theme.semanticTokenColors,
				`no semanticTokenColors in ${t.path}`,
			);
			for (const selector of SEMANTIC_TOKEN_SELECTORS) {
				assert.ok(
					Object.prototype.hasOwnProperty.call(
						theme.semanticTokenColors,
						selector,
					),
					`${t.path} does not paint ${selector}`,
				);
			}
			assert.deepStrictEqual(
				theme.semanticTokenColors["type.declaration"],
				{ bold: true },
				`type declarations are not bold in ${t.path}`,
			);
		}
	});

	test("semantic highlighting defaults on only for game scripts", () => {
		const manifest = JSON.parse(
			fs.readFileSync(path.join(releaseDir, "package.json"), "utf8"),
		) as {
			contributes: {
				configurationDefaults: Record<string, Record<string, unknown>>;
			};
		};
		const defaults = manifest.contributes.configurationDefaults;
		assert.strictEqual(
			defaults["[paradox]"]["editor.semanticHighlighting.enabled"],
			true,
		);
		assert.strictEqual(
			defaults["[cwt]"]?.["editor.semanticHighlighting.enabled"],
			undefined,
		);
	});

	test("every theme has a unique label", () => {
		const labels = themes.map((t) => t.label);
		assert.strictEqual(
			new Set(labels).size,
			labels.length,
			"duplicate theme labels found",
		);
	});

	test("every theme file path is unique", () => {
		const paths = themes.map((t) => t.path);
		assert.strictEqual(
			new Set(paths).size,
			paths.length,
			"duplicate theme file paths found",
		);
	});

	test("every theme has at least one dark and one light variant", () => {
		const types = themes.map((t) => {
			const file = path.join(releaseDir, t.path);
			return (JSON.parse(fs.readFileSync(file, "utf8")) as Theme).type;
		});
		assert.ok(types.includes("dark"), "no dark theme registered");
		assert.ok(types.includes("light"), "no light theme registered");
	});
});

suite("themes — scope coverage", () => {
	test("grammars emit a non-trivial scope set", () => {
		assert.ok(
			required.length > 40,
			`only ${required.length} required scopes found`,
		);
	});

	for (const t of themes) {
		test(`${t.label} paints every grammar scope`, () => {
			const theme = JSON.parse(
				fs.readFileSync(path.join(releaseDir, t.path), "utf8"),
			) as Theme;
			// Minimal themes defer colour to VS Code — coverage check doesn't apply.
			if (theme.minimal) return;
			const rules = themeScopes(theme);
			const uncovered = required.filter((s) => !covers(rules, s));
			assert.strictEqual(
				uncovered.length,
				0,
				`${t.label} leaves ${uncovered.length} scope(s) unstyled: ${uncovered.join(", ")}`,
			);
		});
	}

	test("no theme has duplicate scope rules (same scope painted more than once)", () => {
		for (const t of themes) {
			const theme = JSON.parse(
				fs.readFileSync(path.join(releaseDir, t.path), "utf8"),
			) as Theme;
			const seen = new Set<string>();
			const duplicates: string[] = [];
			for (const rule of theme.tokenColors ?? []) {
				if (!rule.scope) continue;
				const list = Array.isArray(rule.scope)
					? rule.scope
					: rule.scope.split(",");
				for (const raw of list) {
					const s = raw.trim();
					if (s && seen.has(s)) duplicates.push(s);
					else if (s) seen.add(s);
				}
			}
			assert.strictEqual(
				duplicates.length,
				0,
				`${t.label} has duplicate scope rules: ${duplicates.join(", ")}`,
			);
		}
	});
});
