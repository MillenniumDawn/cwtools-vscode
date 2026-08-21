import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

// The workspaceContains events decide whether the extension activates at all
// (#204): a mod laid out without descriptor.mod and with no file matching one
// of these globs never gets a language server. They have to cover the same
// file classes the watchers in lspClient.ts cover, or a perfectly normal mod
// just never activates.

const repoRoot = path.resolve(__dirname, "../../..");
const manifest = JSON.parse(
	fs.readFileSync(
		path.join(repoRoot, "extension", "package", "package.json"),
		"utf8",
	),
) as { activationEvents: string[] };

const lspClientSource = fs.readFileSync(
	path.join(repoRoot, "extension", "src", "host", "lspClient.ts"),
	"utf8",
);

// Pull the same two globs the file watchers use straight from source, so this
// test fails the moment the manifest and the watchers drift apart rather than
// pinning a second hand-copied literal.
const watcherGlobs = [
	...lspClientSource.matchAll(/createFileSystemWatcher\(\s*"([^"]+)"/g),
].map((m) => m[1]);
const scriptGlob = watcherGlobs.find((g) => g.startsWith("**/*.{"));
const locGlob = watcherGlobs.find((g) => g.includes("localisation"));

suite("manifest — activation events", () => {
	test("workspaceContains covers every script and localisation glob the watchers cover", () => {
		assert.ok(scriptGlob, "no script-extension watcher glob found in lspClient.ts");
		assert.ok(locGlob, "no localisation watcher glob found in lspClient.ts");
		assert.ok(
			manifest.activationEvents.includes(`workspaceContains:${scriptGlob}`),
			`activationEvents is missing workspaceContains:${scriptGlob}`,
		);
		assert.ok(
			manifest.activationEvents.includes(`workspaceContains:${locGlob}`),
			`activationEvents is missing workspaceContains:${locGlob}`,
		);
	});

	test("descriptor, metadata, and game-exe events are unchanged", () => {
		const unconditional = [
			"workspaceContains:descriptor.mod",
			"workspaceContains:.metadata/metadata.json",
			"workspaceContains:{,binaries/}{eu4,hoi4,stellaris,CK2,v2game,imperator,ck3,victoria3,eu5}{,.exe}",
		];
		for (const event of unconditional) {
			assert.ok(
				manifest.activationEvents.includes(event),
				`activationEvents is missing ${event}`,
			);
		}
	});
});
