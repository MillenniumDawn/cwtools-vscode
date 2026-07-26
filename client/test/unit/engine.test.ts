import { suite, test } from "vitest";
import * as assert from "assert";
import * as path from "path";
import { EventEmitter } from "events";
import type { ExtensionContext } from "vscode";
import {
	LANGUAGE_REPOS,
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



suite("engine — detectFromFolder", () => {
	const noopExists = () => false;
	const assertDetects = (folder: string, expected: string | null) =>
		assert.strictEqual(detectFromFolder(folder, noopExists), expected);

	test("returns null for an unrecognised folder", () => {
		assertDetects("/home/user/mymod", null);
	});

	test("returns null for an empty path", () => {
		assertDetects("", null);
	});

	test("matches by folder name substring for every supported game", () => {
		assertDetects("/mods/Stellaris_v3", "stellaris");
		assertDetects("/mods/HOI4_Mod", "hoi4");
		assertDetects("/mods/Hearts of Iron", "hoi4");
		assertDetects("/mods/Europa Universalis IV", "eu4");
		assertDetects("/mods/CK2_whatever", "ck2");
		assertDetects("/mods/CK3_whatever", "ck3");
		assertDetects("/mods/CK3_Mod", "ck3");
		assertDetects("/mods/Vic2", "vic2");
		assertDetects("/mods/Victoria 2", "vic2");
		assertDetects("/mods/Vic3", "vic3");
		assertDetects("/mods/Imperator", "imperator");
		assertDetects("/mods/Rome Total War", "imperator");
		assertDetects("/mods/EU5", "eu5");
	});

	test("matching is case-insensitive", () => {
		assertDetects("/mods/stellaris_v3", "stellaris");
		assertDetects("/mods/hoi4_mod", "hoi4");
		assertDetects("/mods/ck3_mod", "ck3");
		assertDetects("/mods/eu5_mod", "eu5");
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
		const contentMarkers: Record<string, string> = {
			"common/ai_strategy": "hoi4",
			"common/species_classes": "stellaris",
			"common/great_projects": "eu4",
			"common/dynasties": "ck3",
		};
		for (const [marker, expectedId] of Object.entries(contentMarkers)) {
			const root = "/opaque";
			const files: Record<string, boolean> = {
				[root + "/" + marker]: true,
			};
			const exists = (p: string) => files[p] === true;
			assert.strictEqual(
				detectFromFolder(root, exists),
				expectedId,
				`marker ${marker} should detect ${expectedId}`,
			);
		}
	});

	test("prefers folder-name hint over content hint", () => {
		const exists = () => true;
		assert.strictEqual(detectFromFolder("/mods/HOI4", exists), "hoi4");
	});

	test("first matching folder-name hint wins (order matters)", () => {
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

	test("trims surrounding whitespace and quotes", () => {
		for (const [raw, expected] of [
			['  "/home/user/config"  ', "/home/user/config"],
			["  '/home/user/config'  ", "/home/user/config"],
		]) {
			const r = resolveRulesFolder(raw, {
				platform: "linux",
				exists: (p) => p === expected,
			});
			assert.strictEqual(r.path, expected);
			assert.strictEqual(r.existed, true);
		}
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
		const raw = "/home/user/./config";
		const r = resolveRulesFolder(raw, {
			platform: "linux",
			exists: (p) => p === raw,
		});
		assert.strictEqual(r.path, raw);
		assert.strictEqual(r.existed, true);
	});
});

suite("engine — serverExe", () => {
	const fakeContext = (abs: string): ExtensionContext =>
		({ asAbsolutePath: (_p: string) => abs }) as unknown as ExtensionContext;

	function runServerExePlatform(
		platform: NodeJS.Platform,
		arch: string,
		expectedSubdir: string,
	) {
		const originalPlatform = Object.getOwnPropertyDescriptor(
			process,
			"platform",
		);
		const originalArch = Object.getOwnPropertyDescriptor(process, "arch");
		Object.defineProperty(process, "platform", { value: platform });
		Object.defineProperty(process, "arch", { value: arch });
		try {
			const ctx = {
				asAbsolutePath: (p: string) => "/ext/" + p,
			} as unknown as ExtensionContext;
			const exe =
				platform === "win32" ? "cwtools-server.exe" : "cwtools-server";
			const expected =
				"/ext/" +
				path.join("bin", "server", "cwtools-server", expectedSubdir, exe);
			const out = serverExe(ctx, (p) => p === expected);
			assert.strictEqual(out, expected);
		} finally {
			if (originalPlatform)
				Object.defineProperty(process, "platform", originalPlatform);
			if (originalArch) Object.defineProperty(process, "arch", originalArch);
		}
	}

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

	test("uses osx-arm64 subdir on darwin arm64", () =>
		runServerExePlatform("darwin", "arm64", "osx-arm64"));
	test("uses osx-x64 subdir on darwin x64", () =>
		runServerExePlatform("darwin", "x64", "osx-x64"));
	test("uses linux-x64 subdir on linux", () =>
		runServerExePlatform("linux", "x64", "linux-x64"));
	// The arm64 vsix carries only that binary, so looking under linux-x64 there
	// finds nothing and the server never starts.
	test("uses linux-arm64 subdir on linux arm64", () =>
		runServerExePlatform("linux", "arm64", "linux-arm64"));
	test("uses win-x64 subdir on windows", () =>
		runServerExePlatform("win32", "x64", "win-x64"));
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

	function makeHangingChild(killFn?: () => void): EventEmitter & {
		stdout: EventEmitter;
		stderr: EventEmitter;
		kill: () => void;
	} {
		const child = new EventEmitter() as EventEmitter & {
			stdout: EventEmitter;
			stderr: EventEmitter;
			kill: () => void;
		};
		child.stdout = new EventEmitter();
		child.stderr = new EventEmitter();
		child.kill = killFn ?? (() => {});
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

	test("times out when git hangs", async () => {
		const fakeSpawn = () => makeHangingChild();
		await assert.rejects(
			() => runGit(["fetch"], fakeSpawn as never, 10),
			/timed out/,
		);
	}, 10000);

	test("timeout kills the child process", async () => {
		let killed = false;
		const fakeSpawn = () =>
			makeHangingChild(() => {
				killed = true;
			});
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
