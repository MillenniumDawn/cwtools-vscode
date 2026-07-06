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
	fs.readFileSync(path.join(repoRoot, "release", "package.json"), "utf8"),
);

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
]);

// Command IDs the client registers via registerCommand('...'), scanned from
// source so the test tracks the code rather than a hand-kept list.
function registeredClientCommands(): Set<string> {
	const dir = path.join(repoRoot, "client", "extension");
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
			(id) => !clientCommands.has(id) && !SERVER_COMMANDS.has(id),
		);
		assert.strictEqual(
			orphans.length,
			0,
			`commands with no handler: ${orphans.join(", ")}`,
		);
	});
});
