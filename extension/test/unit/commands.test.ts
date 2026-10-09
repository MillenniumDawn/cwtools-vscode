import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";
import { SERVER_COMMAND_NAMES } from "../../src/common/serverCommandContract";

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

// Client-required executeCommands come from the shared contract also used by
// dispatch sites; do not keep another list here.
const SERVER_COMMANDS = new Set<string>(SERVER_COMMAND_NAMES);

// Built-in VS Code commands are not server executeCommands and need no client
// registration. Client-owned commands stay in their cwtools. namespace.
const BUILTIN_COMMANDS = new Set(["revealFileInOS", "copyFilePath"]);
const BUILTIN_VSCODE_COMMANDS = new Set([
	"setContext",
	"editor.action.showReferences",
	"workbench.action.reloadWindow",
	"workbench.actions.view.problems",
	...BUILTIN_COMMANDS,
]);

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

// Scan both LSP ExecuteCommandRequest values and VS Code executeCommand calls.
// Raw server names (or unclassified bare ids) bypass the shared server contract;
// built-ins and namespaced client commands are intentionally distinct.
function findUncontractedServerCommandDispatches(
	source: string,
	file: string,
): string[] {
	const uncontracted: string[] = [];
	const requests = /ExecuteCommandRequest\.type,\s*\{([^}]*)\}/gs;
	const requestCommand = /\bcommand\s*:\s*([^,\n}]+)/;
	for (const request of source.matchAll(requests)) {
		const properties = request[1];
		const expression = requestCommand.exec(properties)?.[1].trim();
		const shorthandCommand = /(?:^|,)\s*command\s*(?=,|$)/.test(properties);
		if (expression !== undefined && !expression.startsWith("serverCommand(")) {
			uncontracted.push(`${file}: raw ExecuteCommandRequest command ${expression}`);
		} else if (
			expression === undefined &&
			shorthandCommand &&
			file !== path.join("host", "commandProgress.ts")
		) {
			uncontracted.push(`${file}: untyped ExecuteCommandRequest shorthand command`);
		}
	}

	const vscodeCommand =
		/\b(?:vscode\.)?commands\.executeCommand(?:<[^>]+>)?\s*\(\s*([^,\n)]+)/g;
	for (const match of source.matchAll(vscodeCommand)) {
		const argument = match[1].trim();
		if (argument.startsWith("serverCommand(")) continue;
		const literal = /^(['"])([^'"]+)\1$/.exec(argument);
		if (!literal) {
			uncontracted.push(`${file}: nonliteral VS Code executeCommand argument ${argument}`);
			continue;
		}
		const command = literal[2];
		if (SERVER_COMMANDS.has(command)) {
			uncontracted.push(
				`${file}: raw VS Code dispatch of server command ${command}; use serverCommand()`,
			);
		} else if (
			!BUILTIN_VSCODE_COMMANDS.has(command) &&
			!command.startsWith("cwtools.") &&
			!command.startsWith("cwtools-files.")
		) {
			uncontracted.push(`${file}: unclassified bare VS Code command ${command}`);
		}
	}
	return uncontracted;
}

function sourceTypeScriptFiles(directory: string): string[] {
	return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
		const fullPath = path.join(directory, entry.name);
		if (entry.isDirectory()) return sourceTypeScriptFiles(fullPath);
		return entry.isFile() && entry.name.endsWith(".ts") ? [fullPath] : [];
	});
}

function uncontractedServerCommandDispatches(): string[] {
	const sourceRoot = path.join(repoRoot, "extension", "src");
	return sourceTypeScriptFiles(sourceRoot).flatMap((file) =>
		findUncontractedServerCommandDispatches(
			fs.readFileSync(file, "utf8"),
			path.relative(sourceRoot, file),
		),
	);
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

	test("all server executeCommand dispatches use the shared contract", () => {
		const uncontracted = uncontractedServerCommandDispatches();
		assert.deepStrictEqual(
			uncontracted,
			[],
			`server executeCommand identifiers bypass the shared contract: ${uncontracted.join(", ")}`,
		);
	});

	test("dispatch guard flags raw server ids but permits client and VS Code commands", () => {
		const uncontracted = findUncontractedServerCommandDispatches(
			[
				'commands.executeCommand("getGraphData");',
				'commands.executeCommand("unlistedBareCommand");',
				'commands.executeCommand("setContext");',
				'commands.executeCommand("cwtools.restartServer");',
				'commands.executeCommand(dynamicCommand);',
				'client.sendRequest(ExecuteCommandRequest.type, { command: command });',
				'client.sendRequest(ExecuteCommandRequest.type, { command });',
				'client.sendRequest(ExecuteCommandRequest.type, { command: "getGraphData" });',
			].join("\n"),
			"fixture.ts",
		);
		assert.deepStrictEqual(uncontracted, [
			"fixture.ts: raw ExecuteCommandRequest command command",
			"fixture.ts: untyped ExecuteCommandRequest shorthand command",
			'fixture.ts: raw ExecuteCommandRequest command "getGraphData"',
			"fixture.ts: raw VS Code dispatch of server command getGraphData; use serverCommand()",
			"fixture.ts: unclassified bare VS Code command unlistedBareCommand",
			"fixture.ts: nonliteral VS Code executeCommand argument dynamicCommand",
		]);
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
			"cwtools.formatWorkspace": "cwtoolsFormatWorkspaceAvailable",
			validateWorkspace: "cwtoolsValidateWorkspaceAvailable",
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
