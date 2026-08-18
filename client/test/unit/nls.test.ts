import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

// The manifest reaches its user-visible strings through %key% placeholders
// resolved from package.nls.json. A command or setting whose string is inlined
// instead can never be translated, and a %key% with no entry renders as the
// literal "%key%" in the settings UI. Both used to be true here.
//
// Runtime strings take the other route: vscode.l10n.t() keyed on the English
// string itself, translated in release/l10n/bundle.l10n.<locale>.json. A key
// there that no l10n.t() call passes is dead weight, and a translation that
// drops or renumbers a {0} placeholder loses the value at runtime.

const repoRoot = path.resolve(__dirname, "../../..");
const releaseDir = path.join(repoRoot, "release");
const l10nDir = path.join(releaseDir, "l10n");
const extensionDir = path.join(repoRoot, "client", "extension");

// The nls files are JSONC: translator-hint comments, historically a trailing
// comma. VS Code parses them leniently, so match that rather than failing here.
function loadJsonc(file: string): Record<string, string> {
	const raw = fs.readFileSync(file, "utf8");
	const stripped = raw
		.replace(/^\s*\/\/.*$/gm, "")
		.replace(/,(\s*[}\]])/g, "$1");
	return JSON.parse(stripped) as Record<string, string>;
}

const manifestRaw = fs.readFileSync(
	path.join(releaseDir, "package.json"),
	"utf8",
);
const english = loadJsonc(path.join(releaseDir, "package.nls.json"));
const referenced = new Set(
	[...manifestRaw.matchAll(/"%([^%"]+)%"/g)].map((m) => m[1]),
);

// Discovered, not listed: a new package.nls.<locale>.json is covered the moment
// it lands, with no second place to remember to update.
const TRANSLATIONS = fs
	.readdirSync(releaseDir)
	.filter((f) => /^package\.nls\..+\.json$/.test(f))
	.sort();

const BUNDLES = fs
	.readdirSync(l10nDir)
	.filter((f) => /^bundle\.l10n\..+\.json$/.test(f))
	.sort();

const extensionSources = fs
	.readdirSync(extensionDir)
	.filter((f) => f.endsWith(".ts"))
	.map((f) => fs.readFileSync(path.join(extensionDir, f), "utf8"));

// The first argument of every l10n.t() call, which is the key the bundles are
// written against. Each call passes one string literal, so the source is the
// key list; there is no generated English bundle to drift from it.
function sourceMessages(): string[] {
	const messages: string[] = [];
	for (const src of extensionSources) {
		for (const m of src.matchAll(
			/\bl10n\.t\(\s*(["'])((?:\\.|(?!\1)[^\\])*)\1/g,
		)) {
			messages.push(
				m[2].replace(/\\(.)/g, (_, ch: string) =>
					ch === "n" ? "\n" : ch === "r" ? "\r" : ch === "t" ? "\t" : ch,
				),
			);
		}
	}
	return messages;
}

function callSiteCount(): number {
	return extensionSources.reduce(
		(total, src) => total + [...src.matchAll(/\bl10n\.t\(/g)].length,
		0,
	);
}

function placeholders(text: string): string[] {
	return [...new Set([...text.matchAll(/\{\d+\}/g)].map((m) => m[0]))].sort();
}

suite("nls", () => {
	test("every %key% in the manifest resolves", () => {
		assert.ok(referenced.size > 0, "manifest references no nls keys");
		const missing = [...referenced].filter((k) => !(k in english));
		assert.deepStrictEqual(
			missing,
			[],
			`nls keys with no entry: ${missing.join(", ")}`,
		);
	});

	test("no dead entries left in package.nls.json", () => {
		const dead = Object.keys(english).filter((k) => !referenced.has(k));
		assert.deepStrictEqual(
			dead,
			[],
			`nls entries nothing references: ${dead.join(", ")}`,
		);
	});

	// A stale key in a translation is dead weight; a missing one just falls back
	// to English, which is acceptable for a partial translation.
	for (const file of TRANSLATIONS) {
		test(`${file} has no keys the English file dropped`, () => {
			const translated = loadJsonc(path.join(releaseDir, file));
			const stale = Object.keys(translated).filter((k) => !(k in english));
			assert.deepStrictEqual(
				stale,
				[],
				`stale keys in ${file}: ${stale.join(", ")}`,
			);
		});
	}
});

suite("l10n", () => {
	const source = new Set(sourceMessages());

	test("the extension has runtime strings to translate", () => {
		assert.ok(source.size > 0, "no l10n.t() calls found in client/extension");
	});

	// Everything below is keyed on what the scan above found, so a call the
	// regex can't read is a string that ships untranslated with every test
	// still green. It only reads one string literal, which is why the call
	// sites pass a single (long) literal instead of a template or a
	// concatenation — this is the check that keeps them that way.
	test("every l10n.t() call passes a literal the scan can read", () => {
		assert.strictEqual(
			sourceMessages().length,
			callSiteCount(),
			"an l10n.t() call does not start with a single quoted string literal",
		);
	});

	// A wrong `l10n` path is invisible: the bundles are still valid, the keys
	// still match, and VS Code just never loads them, so every locale silently
	// renders in English.
	test("the manifest points at the directory the bundles are in", () => {
		const manifest = JSON.parse(manifestRaw) as { l10n?: string };
		assert.ok(manifest.l10n, "release/package.json declares no l10n directory");
		const declared = path.resolve(releaseDir, manifest.l10n);
		assert.strictEqual(
			declared,
			l10nDir,
			`l10n points at ${declared}, bundles are in ${l10nDir}`,
		);
		assert.ok(BUNDLES.length > 0, `no bundles in ${manifest.l10n}`);
	});

	test("a translation bundle exists for every manifest locale", () => {
		const locales = (file: string) => file.split(".").slice(2, -1).join(".");
		const missing = TRANSLATIONS.map(locales)
			// The pseudo-locale exercises the manifest path only.
			.filter((loc) => loc !== "qps-ploc")
			.filter((loc) => !BUNDLES.map(locales).includes(loc));
		assert.deepStrictEqual(
			missing,
			[],
			`locales with a manifest translation but no runtime bundle: ${missing.join(", ")}`,
		);
	});

	for (const file of BUNDLES) {
		test(`${file} has no keys no l10n.t() call passes`, () => {
			const bundle = JSON.parse(
				fs.readFileSync(path.join(l10nDir, file), "utf8"),
			) as Record<string, string>;
			const stale = Object.keys(bundle).filter((k) => !source.has(k));
			assert.deepStrictEqual(
				stale,
				[],
				`stale keys in ${file}: ${stale.join(", ")}`,
			);
		});

		test(`${file} keeps every placeholder`, () => {
			const bundle = JSON.parse(
				fs.readFileSync(path.join(l10nDir, file), "utf8"),
			) as Record<string, string>;
			for (const [key, value] of Object.entries(bundle)) {
				assert.deepStrictEqual(
					placeholders(value),
					placeholders(key),
					`${file}: placeholders differ for "${key}"`,
				);
			}
		});
	}
});
