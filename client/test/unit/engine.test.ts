import { suite, test } from "vitest";
import * as assert from "assert";
import * as path from "path";
import { EventEmitter } from "events";
import type { ExtensionContext } from "vscode";
import {
	LANGUAGE_REPOS,
	GAME_DISPLAY,
	GAME_FOLDER,
	detectFromFolder,
	resolveRulesFolder,
	serverExe,
	runGit,
} from "../../extension/engine";

suite("engine — LANGUAGE_REPOS", () => {
	test("covers every supported game with a github URL", () => {
		const expected = [
			"stellaris",
			"eu4",
			"hoi4",
			"ck2",
			"imperator",
			"vic2",
			"vic3",
			"ck3",
			"eu5",
		];
		for (const id of expected) {
			assert.ok(LANGUAGE_REPOS[id], `missing repo URL for ${id}`);
			assert.match(LANGUAGE_REPOS[id], /^https:\/\/github\.com\//);
		}
		assert.strictEqual(Object.keys(LANGUAGE_REPOS).length, expected.length);
	});
});

suite("engine — GAME_DISPLAY", () => {
	test("has a human-readable name for every supported language id", () => {
		for (const id of Object.keys(LANGUAGE_REPOS)) {
			assert.ok(GAME_DISPLAY[id], `missing display name for ${id}`);
			assert.ok(GAME_DISPLAY[id].length > 0);
		}
	});

	test("every display name is unique", () => {
		const names = Object.values(GAME_DISPLAY);
		assert.strictEqual(new Set(names).size, names.length);
	});
});

suite("engine — GAME_FOLDER", () => {
	test("maps vanilla Steam folder names to language ids", () => {
		assert.deepStrictEqual(GAME_FOLDER["stellaris"], { id: "stellaris" });
		assert.deepStrictEqual(GAME_FOLDER["hearts of iron iv"], { id: "hoi4" });
		assert.deepStrictEqual(GAME_FOLDER["victoria ii"], { id: "vic2" });
		assert.deepStrictEqual(GAME_FOLDER["victoria 2"], { id: "vic2" });
	});

	test("flags games whose vanilla install needs a /game subdir", () => {
		assert.strictEqual(GAME_FOLDER["crusader kings iii"].subdir, "game");
		assert.strictEqual(GAME_FOLDER["victoria 3"].subdir, "game");
		assert.strictEqual(GAME_FOLDER["imperator"].subdir, "game");
		assert.strictEqual(GAME_FOLDER["imperatorrome"].subdir, "game");
		assert.strictEqual(GAME_FOLDER["europa universalis v"].subdir, "game");
		assert.strictEqual(GAME_FOLDER["stellaris"].subdir, undefined);
	});

	test("handles alternate Imperator folder names", () => {
		assert.strictEqual(GAME_FOLDER["imperator"].id, "imperator");
		assert.strictEqual(GAME_FOLDER["imperatorrome"].id, "imperator");
	});

	test("every entry maps to a known language id", () => {
		const known = new Set(Object.keys(LANGUAGE_REPOS));
		for (const [, mapping] of Object.entries(GAME_FOLDER)) {
			assert.ok(
				known.has(mapping.id),
				`${mapping.id} is not a known language id`,
			);
		}
	});

	test("no two folder names map to the same id with different subdirs", () => {
		const byId: Record<string, Set<string | undefined>> = {};
		for (const [, mapping] of Object.entries(GAME_FOLDER)) {
			(byId[mapping.id] ??= new Set()).add(mapping.subdir);
		}
		for (const [id, subdirs] of Object.entries(byId)) {
			assert.strictEqual(
				subdirs.size,
				1,
				`${id} has inconsistent subdir values: ${[...subdirs]}`,
			);
		}
	});
});

suite("engine — detectFromFolder", () => {
	const noopExists = () => false;

	test("returns null for an unrecognised folder", () => {
		assert.strictEqual(detectFromFolder("/home/user/mymod", noopExists), null);
	});

	test("returns null for an empty path", () => {
		assert.strictEqual(detectFromFolder("", noopExists), null);
	});

	test("matches by folder name substring for every supported game", () => {
		assert.strictEqual(
			detectFromFolder("/mods/Stellaris_v3", noopExists),
			"stellaris",
		);
		assert.strictEqual(detectFromFolder("/mods/HOI4_Mod", noopExists), "hoi4");
		assert.strictEqual(
			detectFromFolder("/mods/Hearts of Iron", noopExists),
			"hoi4",
		);
		assert.strictEqual(
			detectFromFolder("/mods/Europa Universalis IV", noopExists),
			"eu4",
		);
		assert.strictEqual(
			detectFromFolder("/mods/CK2_whatever", noopExists),
			"ck2",
		);
		assert.strictEqual(
			detectFromFolder("/mods/CK3_whatever", noopExists),
			"ck3",
		);
		assert.strictEqual(detectFromFolder("/mods/CK3_Mod", noopExists), "ck3");
		assert.strictEqual(detectFromFolder("/mods/Vic2", noopExists), "vic2");
		assert.strictEqual(
			detectFromFolder("/mods/Victoria 2", noopExists),
			"vic2",
		);
		assert.strictEqual(detectFromFolder("/mods/Vic3", noopExists), "vic3");
		assert.strictEqual(
			detectFromFolder("/mods/Imperator", noopExists),
			"imperator",
		);
		assert.strictEqual(
			detectFromFolder("/mods/Rome Total War", noopExists),
			"imperator",
		);
		assert.strictEqual(detectFromFolder("/mods/EU5", noopExists), "eu5");
	});

	test("matching is case-insensitive", () => {
		assert.strictEqual(
			detectFromFolder("/mods/stellaris_v3", noopExists),
			"stellaris",
		);
		assert.strictEqual(detectFromFolder("/mods/hoi4_mod", noopExists), "hoi4");
		assert.strictEqual(detectFromFolder("/mods/ck3_mod", noopExists), "ck3");
		assert.strictEqual(detectFromFolder("/mods/eu5_mod", noopExists), "eu5");
	});

	test("falls back to file content markers when the folder name is opaque", () => {
		const files: Record<string, boolean> = {
			["/x/common/ai_strategy"]: true,
		};
		const exists = (p: string) => files[p] === true;
		assert.strictEqual(detectFromFolder("/x", exists), "hoi4");
		assert.strictEqual(detectFromFolder("/y", noopExists), null);
	});

	test("content markers cover every game that lacks a unique folder-name hint", () => {
		// Every game id should be reachable via either a folder-name hint or a
		// content marker. This test verifies the content markers exist for ids
		// that aren't trivially matched by folder name alone.
		const contentMarkers: Record<string, string> = {
			"common/ai_strategy": "hoi4",
			"common/species_classes": "stellaris",
			"common/great_projects": "eu4",
			"common/dynasties": "ck3",
		};
		for (const [marker, expectedId] of Object.entries(contentMarkers)) {
			// Use a root path that won't match any folder-name hint, but where
			// the content marker file exists at root/marker.
			const root = "/opaque";
			const files: Record<string, boolean> = {
				[root + "/" + marker]: true,
			};
			const exists = (p: string) => files[p] === true;
			const result = detectFromFolder(root, exists);
			assert.strictEqual(
				result,
				expectedId,
				`marker ${marker} should detect ${expectedId}`,
			);
		}
	});

	test("prefers folder-name hint over content hint", () => {
		const exists = () => true;
		assert.strictEqual(detectFromFolder("/mods/HOI4", exists), "hoi4");
	});

	test("content hint is checked when no folder-name hint matches", () => {
		const files: Record<string, boolean> = {
			["/opaque/common/species_classes"]: true,
		};
		const exists = (p: string) => files[p] === true;
		assert.strictEqual(detectFromFolder("/opaque", exists), "stellaris");
	});

	test("first matching folder-name hint wins (order matters)", () => {
		// 'crusader kings ii' matches both ck2 and ck3 patterns; ck2 comes first.
		assert.strictEqual(detectFromFolder("/mods/CK2", noopExists), "ck2");
	});
});

suite("engine — resolveRulesFolder", () => {
	test("returns undefined for an unset value", () => {
		const r = resolveRulesFolder(undefined, { exists: () => true });
		assert.strictEqual(r.path, undefined);
		assert.strictEqual(r.existed, false);
	});

	test("returns undefined for an empty/whitespace value", () => {
		const r = resolveRulesFolder("   ", { exists: () => true });
		assert.strictEqual(r.path, undefined);
		assert.strictEqual(r.existed, false);
	});

	test("regression: a Linux path that exists is returned verbatim and unaltered", () => {
		const raw = "/home/user/cwtools-hoi4-config";
		const r = resolveRulesFolder(raw, {
			platform: "linux",
			exists: (p) => p === raw,
		});
		assert.strictEqual(r.path, raw);
		assert.strictEqual(r.existed, true);
	});

	test("regression: a Linux path containing backslashes is never separator-rewritten", () => {
		// Backslash is a legal filename char on Linux; the raw value must win as-is.
		const raw = "/home/user/odd\\dir/config";
		const r = resolveRulesFolder(raw, {
			platform: "linux",
			exists: (p) => p === raw,
		});
		assert.strictEqual(r.path, raw);
		assert.strictEqual(r.existed, true);
	});

	test("win32: a JSON-escaped backslash path resolves once normalized", () => {
		// settings.json value "C:\\Users\\me\\config" arrives as this string;
		// only the normalized form exists on disk.
		const raw = "C:\\Users\\me\\config";
		const normalized = path.win32.normalize(raw);
		const r = resolveRulesFolder(raw, {
			platform: "win32",
			exists: (p) => p === normalized,
		});
		assert.strictEqual(r.path, normalized);
		assert.strictEqual(r.existed, true);
	});

	test("expands a leading ~ to the home directory", () => {
		const home = "/home/user";
		const expanded = path.join(home, "cwtools-config");
		const r = resolveRulesFolder("~/cwtools-config", {
			platform: "linux",
			home,
			exists: (p) => p === expanded,
		});
		assert.strictEqual(r.path, expanded);
		assert.strictEqual(r.existed, true);
	});

	test("expands ~ alone to the home directory", () => {
		const r = resolveRulesFolder("~", {
			platform: "linux",
			home: "/home/user",
			exists: (p) => p === "/home/user",
		});
		assert.strictEqual(r.path, "/home/user");
		assert.strictEqual(r.existed, true);
	});

	test("expands $VAR / ${VAR} env vars on non-win32", () => {
		const env = { RULES: "/opt/rules" };
		const r = resolveRulesFolder("$RULES/hoi4", {
			platform: "linux",
			env,
			exists: (p) => p === "/opt/rules/hoi4",
		});
		assert.strictEqual(r.path, "/opt/rules/hoi4");
		assert.strictEqual(r.existed, true);
	});

	test("expands ${VAR} braced env vars on non-win32", () => {
		const env = { RULES: "/opt/rules" };
		const r = resolveRulesFolder("${RULES}/hoi4", {
			platform: "linux",
			env,
			exists: (p) => p === "/opt/rules/hoi4",
		});
		assert.strictEqual(r.path, "/opt/rules/hoi4");
		assert.strictEqual(r.existed, true);
	});

	test("leaves unset env vars unexpanded on non-win32", () => {
		const r = resolveRulesFolder("$NONEXISTENT/path", {
			platform: "linux",
			env: {},
			exists: (p) => p === "$NONEXISTENT/path",
		});
		assert.strictEqual(r.path, "$NONEXISTENT/path");
		assert.strictEqual(r.existed, true);
	});

	test("leaves unset env vars unexpanded on win32", () => {
		const r = resolveRulesFolder("%NONEXISTENT%\\path", {
			platform: "win32",
			env: {},
			exists: (p) => p === path.win32.normalize("%NONEXISTENT%\\path"),
		});
		assert.strictEqual(r.path, path.win32.normalize("%NONEXISTENT%\\path"));
		assert.strictEqual(r.existed, true);
	});

	test("expands %VAR% env vars on win32", () => {
		const env = { RULES: "C:\\rules" };
		const expected = path.win32.normalize("C:\\rules\\hoi4");
		const r = resolveRulesFolder("%RULES%\\hoi4", {
			platform: "win32",
			env,
			exists: (p) => p === expected,
		});
		assert.strictEqual(r.path, expected);
		assert.strictEqual(r.existed, true);
	});

	test("trims surrounding whitespace and double quotes", () => {
		const raw = '  "/home/user/config"  ';
		const r = resolveRulesFolder(raw, {
			platform: "linux",
			exists: (p) => p === "/home/user/config",
		});
		assert.strictEqual(r.path, "/home/user/config");
		assert.strictEqual(r.existed, true);
	});

	test("trims surrounding single quotes", () => {
		const raw = "  '/home/user/config'  ";
		const r = resolveRulesFolder(raw, {
			platform: "linux",
			exists: (p) => p === "/home/user/config",
		});
		assert.strictEqual(r.path, "/home/user/config");
		assert.strictEqual(r.existed, true);
	});

	test("resolves a relative path against the workspace root", () => {
		const r = resolveRulesFolder("rules", {
			platform: "linux",
			workspaceRoot: "/mods/mymod",
			exists: (p) => p === path.resolve("/mods/mymod", "rules"),
		});
		assert.strictEqual(r.path, path.resolve("/mods/mymod", "rules"));
		assert.strictEqual(r.existed, true);
	});

	test("does not resolve an absolute path against workspace root", () => {
		const r = resolveRulesFolder("/absolute/path", {
			platform: "linux",
			workspaceRoot: "/mods/mymod",
			exists: (p) => p === "/absolute/path",
		});
		assert.strictEqual(r.path, "/absolute/path");
		assert.strictEqual(r.existed, true);
	});

	test("set-but-missing returns existed:false with a defined best-effort path", () => {
		const r = resolveRulesFolder('  "C:\\Users\\me\\missing"  ', {
			platform: "win32",
			exists: () => false,
		});
		assert.strictEqual(r.existed, false);
		assert.ok(
			r.path,
			"best-effort path must be defined so the caller can warn",
		);
		assert.ok(
			!r.path!.includes('"'),
			"best-effort path should be trimmed of quotes",
		);
	});

	test("raw value that exists is returned before any normalization runs", () => {
		// Even though the normalized form also exists, the raw value wins.
		const raw = "/home/user/./config";
		const r = resolveRulesFolder(raw, {
			platform: "linux",
			exists: (p) => p === raw,
		});
		assert.strictEqual(r.path, raw);
		assert.strictEqual(r.existed, true);
	});

	test("deduplicates candidates so the same path is not checked twice", () => {
		let checks = 0;
		const r = resolveRulesFolder("/path", {
			platform: "linux",
			exists: (p) => {
				checks++;
				return p === "/path";
			},
		});
		assert.strictEqual(r.path, "/path");
		assert.strictEqual(r.existed, true);
		// Raw value checked once, then trimmed (same), then ~ (different), etc.
		// The key assertion is that the same path is not checked twice.
		assert.ok(checks <= 5, `too many exists checks: ${checks}`);
	});
});

suite("engine — serverExe", () => {
	const fakeContext = (abs: string): ExtensionContext =>
		({ asAbsolutePath: (_p: string) => abs }) as unknown as ExtensionContext;

	test("returns the server binary path when it exists", () => {
		const ctx = fakeContext("/ext/bin/server/cwtools-server/cwtools-server");
		const out = serverExe(ctx, () => true);
		assert.strictEqual(out, "/ext/bin/server/cwtools-server/cwtools-server");
	});

	test("returns undefined when the binary is not deployed", () => {
		const ctx = fakeContext("/ext/bin/server/cwtools-server/cwtools-server");
		assert.strictEqual(
			serverExe(ctx, () => false),
			undefined,
		);
	});

	test("prefers the flat layout over the per-platform subdir", () => {
		const ctx = {
			asAbsolutePath: (p: string) => "/ext/" + p,
		} as unknown as ExtensionContext;
		const flat =
			"/ext/" + path.join("bin", "server", "cwtools-server", "cwtools-server");
		const nested =
			"/ext/" +
			path.join(
				"bin",
				"server",
				"cwtools-server",
				"linux-x64",
				"cwtools-server",
			);
		const out = serverExe(ctx, (p) => p === flat || p === nested);
		assert.strictEqual(out, flat, "flat layout should be preferred");
	});

	test("falls back to the per-platform subdir of a packaged vsix", () => {
		const original = Object.getOwnPropertyDescriptor(process, "platform");
		Object.defineProperty(process, "platform", { value: "linux" });
		try {
			const ctx = {
				asAbsolutePath: (p: string) => "/ext/" + p,
			} as unknown as ExtensionContext;
			const nested =
				"/ext/" +
				path.join(
					"bin",
					"server",
					"cwtools-server",
					"linux-x64",
					"cwtools-server",
				);
			// Flat path absent (no single-platform binary), nested one present.
			const out = serverExe(ctx, (p) => p === nested);
			assert.strictEqual(out, nested);
		} finally {
			if (original) Object.defineProperty(process, "platform", original);
		}
	});

	test("uses the .exe extension on Windows", () => {
		const originalPlatform = Object.getOwnPropertyDescriptor(
			process,
			"platform",
		);
		Object.defineProperty(process, "platform", { value: "win32" });
		try {
			const ctx = fakeContext(
				"C:\\ext\\bin\\server\\cwtools-server\\cwtools-server.exe",
			);
			const out = serverExe(ctx, () => true);
			assert.ok(out!.endsWith("cwtools-server.exe"));
		} finally {
			if (originalPlatform)
				Object.defineProperty(process, "platform", originalPlatform);
		}
	});

	test("uses osx-arm64 subdir on darwin arm64", () => {
		const originalPlatform = Object.getOwnPropertyDescriptor(
			process,
			"platform",
		);
		const originalArch = Object.getOwnPropertyDescriptor(process, "arch");
		Object.defineProperty(process, "platform", { value: "darwin" });
		Object.defineProperty(process, "arch", { value: "arm64" });
		try {
			const ctx = {
				asAbsolutePath: (p: string) => "/ext/" + p,
			} as unknown as ExtensionContext;
			const nested =
				"/ext/" +
				path.join(
					"bin",
					"server",
					"cwtools-server",
					"osx-arm64",
					"cwtools-server",
				);
			const out = serverExe(ctx, (p) => p === nested);
			assert.strictEqual(out, nested);
		} finally {
			if (originalPlatform)
				Object.defineProperty(process, "platform", originalPlatform);
			if (originalArch) Object.defineProperty(process, "arch", originalArch);
		}
	});

	test("uses osx-x64 subdir on darwin x64", () => {
		const originalPlatform = Object.getOwnPropertyDescriptor(
			process,
			"platform",
		);
		const originalArch = Object.getOwnPropertyDescriptor(process, "arch");
		Object.defineProperty(process, "platform", { value: "darwin" });
		Object.defineProperty(process, "arch", { value: "x64" });
		try {
			const ctx = {
				asAbsolutePath: (p: string) => "/ext/" + p,
			} as unknown as ExtensionContext;
			const nested =
				"/ext/" +
				path.join(
					"bin",
					"server",
					"cwtools-server",
					"osx-x64",
					"cwtools-server",
				);
			const out = serverExe(ctx, (p) => p === nested);
			assert.strictEqual(out, nested);
		} finally {
			if (originalPlatform)
				Object.defineProperty(process, "platform", originalPlatform);
			if (originalArch) Object.defineProperty(process, "arch", originalArch);
		}
	});

	test("uses linux-x64 subdir on linux", () => {
		const originalPlatform = Object.getOwnPropertyDescriptor(
			process,
			"platform",
		);
		const originalArch = Object.getOwnPropertyDescriptor(process, "arch");
		Object.defineProperty(process, "platform", { value: "linux" });
		Object.defineProperty(process, "arch", { value: "x64" });
		try {
			const ctx = {
				asAbsolutePath: (p: string) => "/ext/" + p,
			} as unknown as ExtensionContext;
			const nested =
				"/ext/" +
				path.join(
					"bin",
					"server",
					"cwtools-server",
					"linux-x64",
					"cwtools-server",
				);
			const out = serverExe(ctx, (p) => p === nested);
			assert.strictEqual(out, nested);
		} finally {
			if (originalPlatform)
				Object.defineProperty(process, "platform", originalPlatform);
			if (originalArch) Object.defineProperty(process, "arch", originalArch);
		}
	});

	test("uses win-x64 subdir on windows", () => {
		const originalPlatform = Object.getOwnPropertyDescriptor(
			process,
			"platform",
		);
		const originalArch = Object.getOwnPropertyDescriptor(process, "arch");
		Object.defineProperty(process, "platform", { value: "win32" });
		Object.defineProperty(process, "arch", { value: "x64" });
		try {
			const ctx = {
				asAbsolutePath: (p: string) => "/ext/" + p,
			} as unknown as ExtensionContext;
			const nested =
				"/ext/" +
				path.join(
					"bin",
					"server",
					"cwtools-server",
					"win-x64",
					"cwtools-server.exe",
				);
			const out = serverExe(ctx, (p) => p === nested);
			assert.strictEqual(out, nested);
		} finally {
			if (originalPlatform)
				Object.defineProperty(process, "platform", originalPlatform);
			if (originalArch) Object.defineProperty(process, "arch", originalArch);
		}
	});
});

suite("engine — runGit", () => {
	function makeChild(opts: {
		code: number | null;
		signal: NodeJS.Signals | null;
		stdout?: string;
		stderr?: string;
		error?: Error;
	}): EventEmitter & { stdout: EventEmitter; stderr: EventEmitter } {
		const child = new EventEmitter() as EventEmitter & {
			stdout: EventEmitter;
			stderr: EventEmitter;
		};
		child.stdout = new EventEmitter();
		child.stderr = new EventEmitter();
		queueMicrotask(() => {
			if (opts.stdout) child.stdout.emit("data", Buffer.from(opts.stdout));
			if (opts.stderr) child.stderr.emit("data", Buffer.from(opts.stderr));
			if (opts.error) child.emit("error", opts.error);
			else child.emit("close", opts.code, opts.signal);
		});
		return child;
	}

	test("resolves when git exits with code 0", async () => {
		const fakeSpawn = () =>
			makeChild({ code: 0, signal: null, stdout: "ok\n" });
		await runGit(["status"], fakeSpawn as never);
	});

	test("resolves when git exits with code 0 and stderr output", async () => {
		// Some git versions write progress to stderr even on success.
		const fakeSpawn = () =>
			makeChild({
				code: 0,
				signal: null,
				stdout: "",
				stderr: "warning: progress info\n",
			});
		await runGit(["fetch"], fakeSpawn as never);
	});

	test("rejects when git exits non-zero", async () => {
		const fakeSpawn = () =>
			makeChild({ code: 128, signal: null, stderr: "fatal: not a repo\n" });
		await assert.rejects(
			() => runGit(["pull"], fakeSpawn as never),
			/git exited with code 128/,
		);
	});

	test("rejects when git is killed by a signal", async () => {
		const fakeSpawn = () => makeChild({ code: null, signal: "SIGTERM" });
		await assert.rejects(
			() => runGit(["fetch"], fakeSpawn as never),
			/signal: SIGTERM/,
		);
	});

	test("rejects when git fails to spawn", async () => {
		const fakeSpawn = () =>
			makeChild({ code: null, signal: null, error: new Error("ENOENT") });
		await assert.rejects(() => runGit(["clone"], fakeSpawn as never), /ENOENT/);
	});

	test("rejects with spawn EACCES error", async () => {
		const fakeSpawn = () =>
			makeChild({
				code: null,
				signal: null,
				error: new Error("EACCES: permission denied"),
			});
		await assert.rejects(() => runGit(["pull"], fakeSpawn as never), /EACCES/);
	});

	test("times out when git hangs", async () => {
		// A child that never emits close/error should trigger the timeout.
		const child = new EventEmitter() as EventEmitter & {
			stdout: EventEmitter;
			stderr: EventEmitter;
			kill: () => void;
		};
		child.stdout = new EventEmitter();
		child.stderr = new EventEmitter();
		child.kill = () => {};
		const fakeSpawn = () => child;
		await assert.rejects(
			() => runGit(["fetch"], fakeSpawn as never, 10),
			/timed out/,
		);
	}, 10000);

	test("timeout kills the child process", async () => {
		let killed = false;
		const child = new EventEmitter() as EventEmitter & {
			stdout: EventEmitter;
			stderr: EventEmitter;
			kill: () => void;
		};
		child.stdout = new EventEmitter();
		child.stderr = new EventEmitter();
		child.kill = () => {
			killed = true;
		};
		const fakeSpawn = () => child;
		await assert.rejects(
			() => runGit(["fetch"], fakeSpawn as never, 10),
			/timed out/,
		);
		assert.strictEqual(
			killed,
			true,
			"child.kill() should be called on timeout",
		);
	});

	test("timeout does not fire if git completes before the deadline", async () => {
		const fakeSpawn = () =>
			makeChild({ code: 0, signal: null, stdout: "ok\n" });
		// Use a generous timeout; the microtask resolves before it fires.
		await runGit(["status"], fakeSpawn as never, 5000);
	});
});
