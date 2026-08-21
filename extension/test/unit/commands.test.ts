import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

// Guards against declaring a command-palette entry with no handler — the class
// of bug where dead cwtools commands lingered in the manifest, each erroring
// "command not found" when run. Every contributed command must be registered
// client-side or advertised by the server.

const repoRoot = path.resolve(__dirname, "../../..");
const manifest = JSON.parse(
	fs.readFileSync(
		path.join(repoRoot, "extension", "package", "package.json"),
		"utf8",
	),
) as {
	contributes: {
		commands?: Array<{ command: string }>;
		menus?: { commandPalette?: Array<{ command: string; when?: string }> };
	};
};

// executeCommands the server advertises (config.rs execute_command_provider),
// including genlocall and reloadrulesconfig, which the server now handles.
const SERVER_COMMANDS = new Set([
	"getFileTypes",
	"exportProfilingLog",
	"cacheVanilla",
	"clearAllCaches",
	"reindexWorkspace",
	"genlocall",
	"reloadrulesconfig",
	"getGraphData",
]);

// Built-in VS Code commands the manifest references in menus (declared so the
// extension host doesn't warn, hidden from the palette). They need no client
// handler and aren't server-advertised.
const BUILTIN_COMMANDS = new Set(["revealFileInOS", "copyFilePath"]);

// Command IDs the client registers via registerCommand('...'), scanned from
// source so the test tracks the code rather than a hand-kept list.
function registeredClientCommands(): Set<string> {
	const dir = path.join(repoRoot, "extension", "src", "host");
	const ids = new Set<string>();
	const re = /registerCommand\(\s*['"]([^'"]+)['"]/g;
	for (const file of fs.readdirSync(dir)) {
		if (!file.endsWith(".ts")) continue;
		const src = fs.readFileSync(path.join(dir, file), "utf8");
		for (const m of src.matchAll(re)) ids.add(m[1]);
	}
	return ids;
}

const contributed: string[] = (manifest.contributes.commands ?? []).map(
	(c: { command: string }) => c.command,
);
const clientCommands = registeredClientCommands();

suite("manifest — command registration", () => {
	test("every contributed command is registered client-side or server-advertised", () => {
		assert.ok(contributed.length > 0, "no commands contributed");
		const orphans = contributed.filter(
			(id) =>
				!clientCommands.has(id) &&
				!SERVER_COMMANDS.has(id) &&
				!BUILTIN_COMMANDS.has(id),
		);
		assert.strictEqual(
			orphans.length,
			0,
			`commands with no handler: ${orphans.join(", ")}`,
		);
	});

	// The graph commands go through the server's getGraphData, and the workspace
	// auto-fix runs the server's fixAllWorkspace. Each gate reads the running
	// server's advertised capabilities, not what the newest engine can do, so it
	// still matters after the command lands: someone on an older server still
	// needs them hidden rather than dead-ending in "command not found".
	// Asserted unconditionally — an earlier version skipped the whole check once
	// the command was known, which made it pass without testing anything.
	test("capability-gated commands are gated in the palette", () => {
		const palette: Array<{ command: string; when?: string }> =
			manifest.contributes.menus?.commandPalette ?? [];
		const gated: Record<string, string> = {
			"cwtools.showGraph": "cwtoolsGraphAvailable",
			"cwtools.setGraphDepth": "cwtoolsGraphAvailable",
			"cwtools.fixAllWorkspace": "cwtoolsFixAllAvailable",
		};
		for (const [id, key] of Object.entries(gated)) {
			const entry = palette.find((e) => e.command === id);
			assert.ok(
				entry,
				`${id} has no commandPalette entry, so it shows unconditionally`,
			);
			assert.match(
				entry.when ?? "",
				new RegExp(key),
				`${id} is not gated on ${key}`,
			);
		}
	});

	// Client-owned command IDs live in the cwtools. namespace so they can't
	// collide with other extensions; only server-advertised IDs stay bare.
	// Checked on the registration side, so a bare ID can't hide by skipping
	// the manifest.
	test("every client-registered command is namespaced under cwtools.", () => {
		// View-id-prefixed tree-item command; the cwtools-files view id is the prefix.
		const VIEW_SCOPED_COMMANDS = new Set([
			"cwtools-files.openFile",
			"cwtools-files.revealActiveFile",
		]);
		const bare = [...clientCommands].filter(
			(id) =>
				!id.startsWith("cwtools.") &&
				!SERVER_COMMANDS.has(id) &&
				!VIEW_SCOPED_COMMANDS.has(id),
		);
		assert.strictEqual(
			bare.length,
			0,
			`client commands outside the cwtools. namespace: ${bare.join(", ")}`,
		);
	});
});
