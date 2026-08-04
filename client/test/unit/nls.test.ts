import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

// The manifest reaches its user-visible strings through %key% placeholders
// resolved from package.nls.json. A command or setting whose string is inlined
// instead can never be translated, and a %key% with no entry renders as the
// literal "%key%" in the settings UI. Both used to be true here.

const repoRoot = path.resolve(__dirname, "../../..");
const releaseDir = path.join(repoRoot, "release");

// The nls files are JSONC: translator-hint comments, historically a trailing
// comma. VS Code parses them leniently, so match that rather than failing here.
function loadJsonc(file: string): Record<string, string> {
	const raw = fs.readFileSync(file, "utf8");
	const stripped = raw
		.replace(/^\s*\/\/.*$/gm, "")
		.replace(/,(\s*[}\]])/g, "$1");
	return JSON.parse(stripped) as Record<string, string>;
}

const manifestRaw = fs.readFileSync(path.join(releaseDir, "package.json"), "utf8");
const english = loadJsonc(path.join(releaseDir, "package.nls.json"));
const referenced = new Set(
	[...manifestRaw.matchAll(/"%([^%"]+)%"/g)].map((m) => m[1]),
);

const TRANSLATIONS = ["package.nls.zh.json", "package.nls.qps-ploc.json"];

suite("nls", () => {
	test("every %key% in the manifest resolves", () => {
		assert.ok(referenced.size > 0, "manifest references no nls keys");
		const missing = [...referenced].filter((k) => !(k in english));
		assert.deepStrictEqual(missing, [], `nls keys with no entry: ${missing.join(", ")}`);
	});

	test("no dead entries left in package.nls.json", () => {
		const dead = Object.keys(english).filter((k) => !referenced.has(k));
		assert.deepStrictEqual(dead, [], `nls entries nothing references: ${dead.join(", ")}`);
	});

	// A stale key in a translation is dead weight; a missing one just falls back
	// to English, which is acceptable for a partial translation.
	for (const file of TRANSLATIONS) {
		test(`${file} has no keys the English file dropped`, () => {
			const translated = loadJsonc(path.join(releaseDir, file));
			const stale = Object.keys(translated).filter((k) => !(k in english));
			assert.deepStrictEqual(stale, [], `stale keys in ${file}: ${stale.join(", ")}`);
		});
	}
});
