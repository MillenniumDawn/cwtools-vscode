import { suite, test, vi } from "vitest";
import * as assert from "assert";

const logged: { level: string; message: string }[] = [];
vi.mock("../../src/host/logger", async () =>
	(await import("./support/loggerMock")).mockLogger({
		logInfo: (m: string) => logged.push({ level: "info", message: m }),
		logWarn: (m: string) => logged.push({ level: "warn", message: m }),
		logError: (m: string) => logged.push({ level: "error", message: m }),
	}),
);
import * as path from "path";
import { existsSync } from "fs";
import { EventEmitter } from "events";
import type { ExtensionContext } from "vscode";
import {
	LANGUAGE_REPOS,
	detectFromFolder,
	resolveRulesFolder,
	serverExe,
	runGit,
	rulesFetchCommands,
	GitNotFoundError,
} from "../../src/host/engine";
import type { ResolveRulesFolderOpts } from "../../src/host/engine";
import { GAMES } from "../../src/host/games";

suite("engine — game table", () => {
	test("every game has a unique id and a pinned github repo", () => {
		assert.strictEqual(new Set(GAMES.map((g) => g.id)).size, GAMES.length);
		assert.deepStrictEqual(
			Object.keys(LANGUAGE_REPOS).sort(),
			GAMES.map((g) => g.id).sort(),
		);
		for (const g of GAMES) {
			assert.match(g.repo, /^https:\/\/github\.com\//, `${g.id} repo`);
			assert.match(g.repoRef, /^[0-9a-f]{40}$/, `${g.id} repoRef`);
			assert.deepStrictEqual(LANGUAGE_REPOS[g.id], {
				repo: g.repo,
				ref: g.repoRef,
			});
		}
	});
});

suite("engine — detectFromFolder", () => {
	const noopExists = () => false;
	const assertDetects = async (folder: string, expected: string | null) =>
		assert.strictEqual(await detectFromFolder(folder, noopExists), expected);

	// Newer games share a prefix with their predecessors, so the pairs are here
	// together: a hint table in the wrong order sends CK3 to ck2.
	test.each([
		["/home/user/mymod", null],
		["", null],
		["/mods/Stellaris_v3", "stellaris"],
		["/mods/hoi4_mod", "hoi4"],
		["/mods/Hearts of Iron", "hoi4"],
		["/mods/Europa Universalis IV", "eu4"],
		["/mods/eu4", "eu4"],
		["/mods/EU5", "eu5"],
		["/x/Documents/Paradox Interactive/Europa Universalis V/mod/m", "eu5"],
		["/mods/CK2_whatever", "ck2"],
		["/x/Crusader Kings II", "ck2"],
		["/mods/ck3_mod", "ck3"],
		["/x/Crusader Kings III", "ck3"],
		["/mods/Vic2", "vic2"],
		["/mods/Victoria 2", "vic2"],
		["/x/Victoria II", "vic2"],
		["/mods/Vic3", "vic3"],
		["/x/Victoria III", "vic3"],
		["/mods/Imperator", "imperator"],
		["/mods/Rome Total War", "imperator"],
	])("folder name %s detects %s", async (folder, expected) => {
		await assertDetects(folder, expected);
	});

	test.each([
		["common/ai_strategy", "hoi4"],
		["common/species_classes", "stellaris"],
		["common/great_projects", "eu4"],
		["common/dynasties", "ck3"],
	])("content marker %s detects %s under an opaque name", async (marker, expected) => {
		assert.strictEqual(
			await detectFromFolder("/opaque", (p) => p === "/opaque/" + marker),
			expected,
		);
	});

	test("content beats a conflicting root name", async () => {
		const root = path.resolve("mods", "Stellaris");
		assert.strictEqual(
			await detectFromFolder(root, (p) => p === path.join(root, "common", "ai_strategy")),
			"hoi4",
		);
	});

	test.each([
		"/games/Crusader Kings II",
		"C:\\games\\Crusader Kings II\\",
		"/mods/CK2_dynasty_mod",
		"C:\\mods\\CK2_dynasty_mod",
		"/games/Crusader Kings II/mod/opaque",
		"C:\\games\\Crusader Kings II\\mod\\opaque",
		"/games/Crusader Kings II/game",
		"C:\\games\\Crusader Kings II\\game",
	])("shared dynasties content preserves the recognized CK2 root or layout: %s", async (root) => {
		assert.strictEqual(await detectFromFolder(root, (p) =>
			p === path.join(root, "common", "dynasties")), "ck2");
	});

	test.each([
		"/games/Crusader Kings II",
		"/games/Crusader Kings II/mod/opaque",
		"/home/ck2/projects/opaque",
		"C:\\Users\\Crusader Kings II\\projects\\opaque",
	])("HOI4 content keeps priority over CK2 names and unrelated ancestors: %s", async (root) => {
		assert.strictEqual(await detectFromFolder(root, (p) =>
			p === path.join(root, "common", "dynasties") ||
			p === path.join(root, "common", "ai_strategy")), "hoi4");
	});

	test.each([
		"/home/ck2/projects/opaque",
		"C:\\Users\\Crusader Kings II\\projects\\opaque",
		"/games/Crusader Kings III",
		"/games/Crusader Kings III/mod/opaque",
	])("shared dynasties default to CK3 without a recognized CK2 root or layout: %s", async (root) => {
		assert.strictEqual(await detectFromFolder(root, (p) =>
			p === path.join(root, "common", "dynasties")), "ck3");
	});

	test("mixed content markers follow game-table order before names", async () => {
		assert.strictEqual(await detectFromFolder("/mods/HOI4", () => true), "stellaris");
	});

	test.each([
		"/home/stellaris/mods/Millennium-Dawn",
		"/home/europa/projects/Millennium-Dawn/",
		"C:\\Users\\stellaris\\mods\\Millennium-Dawn",
		"C:\\projects\\europa\\Millennium-Dawn\\",
	])("unrelated ancestors do not provide folder hints: %s", async (root) => {
		await assertDetects(root, null);
		assert.strictEqual(await detectFromFolder(root, (p) =>
			p.replace(/\\/g, "/").endsWith("/common/ai_strategy")), "hoi4");
	});

	test.each([
		["/games/Crusader Kings III/game", "ck3"],
		["C:\\games\\Crusader Kings III\\game\\", "ck3"],
		["/projects/stellaris/game", "stellaris"],
		["/projects/stellaris-project/mod/opaque", null],
		["C:\\Documents\\Paradox Interactive\\Hearts of Iron IV\\mod\\opaque", "hoi4"],
		// The layout's game wins over another game's keyword in the mod name.
		["/Documents/Paradox Interactive/Hearts of Iron IV/mod/Fall of Rome", "hoi4"],
		["C:\\Documents\\Paradox Interactive\\Hearts of Iron IV\\mod\\Europa Reborn", "hoi4"],
		["/Documents/Paradox Interactive/Crusader Kings III/mod/ck2_conversion", "ck3"],
		["/Documents/Paradox Interactive/Stellaris/mod/Victoria 3 flags", "stellaris"],
	])("canonical mod and game-subdirectory layout %s detects %s", async (root, expected) => {
		await assertDetects(root, expected);
	});

	// The host label's workspace has no game name in its path, so it must carry
	// the Stellaris `common/species_classes` content marker for the hover and
	// completion suites to see real rule data instead of the paradox fallback.
	test("the sample host fixture detects as stellaris via its content marker", async () => {
		const sampleRoot = path.resolve(__dirname, "../workspaces/stellaris");
		assert.strictEqual(
			await detectFromFolder(sampleRoot, (p) => existsSync(p)),
			"stellaris",
		);
	});
});

suite("engine — resolveRulesFolder", () => {
	test.each([undefined, "   "])("an unset or blank value %j resolves to nothing", (raw) => {
		assert.deepStrictEqual(resolveRulesFolder(raw, { exists: () => true }), {
			path: undefined,
			existed: false,
		});
	});

	const linux: ResolveRulesFolderOpts = { platform: "linux" };
	const win32: ResolveRulesFolderOpts = { platform: "win32" };
	const home = "/home/user";

	// Each row names the only path that exists on disk; the raw setting has to
	// resolve to exactly that.
	test.each<[string, string, ResolveRulesFolderOpts, string]>([
		["an existing path is returned verbatim", "/home/user/cwtools-hoi4-config", linux, "/home/user/cwtools-hoi4-config"],
		// Backslash is a legal filename char on Linux; the raw value must win as-is.
		["a Linux path with backslashes is never separator-rewritten", "/home/user/odd\\dir/config", linux, "/home/user/odd\\dir/config"],
		["the raw value wins before normalization", "/home/user/./config", linux, "/home/user/./config"],
		// settings.json "C:\\Users\\me\\config" arrives as this string; only the
		// normalized form exists on disk.
		["win32 normalizes a JSON-escaped backslash path", "C:\\Users\\me\\config", win32, path.win32.normalize("C:\\Users\\me\\config")],
		["a leading ~ expands to home", "~/cwtools-config", { ...linux, home }, path.join(home, "cwtools-config")],
		["~ alone expands to home", "~", { ...linux, home }, home],
		["$VAR expands off win32", "$RULES/hoi4", { ...linux, env: { RULES: "/opt/rules" } }, "/opt/rules/hoi4"],
		["${VAR} expands off win32", "${RULES}/hoi4", { ...linux, env: { RULES: "/opt/rules" } }, "/opt/rules/hoi4"],
		["an unset $VAR stays literal", "$NONEXISTENT/path", { ...linux, env: {} }, "$NONEXISTENT/path"],
		["%VAR% expands on win32", "%RULES%\\hoi4", { ...win32, env: { RULES: "C:\\rules" } }, path.win32.normalize("C:\\rules\\hoi4")],
		["an unset %VAR% stays literal", "%NONEXISTENT%\\path", { ...win32, env: {} }, path.win32.normalize("%NONEXISTENT%\\path")],
		["double quotes and whitespace are trimmed", '  "/home/user/config"  ', linux, "/home/user/config"],
		["single quotes and whitespace are trimmed", "  '/home/user/config'  ", linux, "/home/user/config"],
		["a relative path resolves against the workspace root", "rules", { ...linux, workspaceRoot: "/mods/mymod" }, path.resolve("/mods/mymod", "rules")],
		["an absolute path ignores the workspace root", "/absolute/path", { ...linux, workspaceRoot: "/mods/mymod" }, "/absolute/path"],
	])("%s", (_name, raw, opts, expected) => {
		assert.deepStrictEqual(
			resolveRulesFolder(raw, { ...opts, exists: (p) => p === expected }),
			{ path: expected, existed: true },
		);
	});

	test("set-but-missing returns existed:false with a trimmed best-effort path", () => {
		const r = resolveRulesFolder('  "C:\\Users\\me\\missing"  ', {
			...win32,
			exists: () => false,
		});
		assert.strictEqual(r.existed, false);
		assert.strictEqual(r.path, path.win32.normalize("C:\\Users\\me\\missing"));
	});
});

suite("engine — serverExe", () => {
	const context = {
		asAbsolutePath: (p: string) => "/ext/" + p,
	} as unknown as ExtensionContext;
	const serverDir = path.join("bin", "server", "cwtools-server");

	function onPlatform<T>(platform: NodeJS.Platform, arch: string, run: () => T): T {
		const originalPlatform = Object.getOwnPropertyDescriptor(process, "platform");
		const originalArch = Object.getOwnPropertyDescriptor(process, "arch");
		Object.defineProperty(process, "platform", { value: platform });
		Object.defineProperty(process, "arch", { value: arch });
		try {
			return run();
		} finally {
			if (originalPlatform) Object.defineProperty(process, "platform", originalPlatform);
			if (originalArch) Object.defineProperty(process, "arch", originalArch);
		}
	}

	test("returns undefined when the binary is not deployed", () => {
		assert.strictEqual(serverExe(context, () => false), undefined);
	});

	test("prefers the flat layout over the per-platform subdir", () => {
		const flat = "/ext/" + path.join(serverDir, "cwtools-server");
		assert.strictEqual(
			onPlatform("linux", "x64", () => serverExe(context, () => true)),
			flat,
		);
	});

	// A packaged vsix carries one binary under its platform's subdir. The arm64
	// one in particular finds nothing under linux-x64 and never starts.
	test.each<[NodeJS.Platform, string, string, string]>([
		["darwin", "arm64", "osx-arm64", "cwtools-server"],
		["darwin", "x64", "osx-x64", "cwtools-server"],
		["linux", "x64", "linux-x64", "cwtools-server"],
		["linux", "arm64", "linux-arm64", "cwtools-server"],
		["win32", "x64", "win-x64", "cwtools-server.exe"],
	])("falls back to the packaged subdir on %s %s", (platform, arch, subdir, exe) => {
		const nested = "/ext/" + path.join(serverDir, subdir, exe);
		assert.strictEqual(
			onPlatform(platform, arch, () => serverExe(context, (p) => p === nested)),
			nested,
		);
	});
});

suite("engine — runGit", () => {
	type FakeChild = EventEmitter & {
		stdout: EventEmitter;
		stderr: EventEmitter;
		kill: () => void;
	};

	// With no `end`, the child hangs until the timeout kills it.
	function spawning(opts: {
		stdout?: string;
		stderr?: string;
		end?: () => unknown[][];
		kill?: () => void;
	}): never {
		return (() => {
			const child = new EventEmitter() as FakeChild;
			child.stdout = new EventEmitter();
			child.stderr = new EventEmitter();
			child.kill = opts.kill ?? (() => {});
			queueMicrotask(() => {
				if (opts.stdout) child.stdout.emit("data", Buffer.from(opts.stdout));
				if (opts.stderr) child.stderr.emit("data", Buffer.from(opts.stderr));
				for (const [event, ...args] of opts.end?.() ?? []) {
					child.emit(event as string, ...args);
				}
			});
			return child;
		}) as never;
	}
	const close = (code: number | null, signal: string | null = null) => () => [
		["close", code, signal],
	];
	const enoent = () =>
		Object.assign(new Error("spawn git ENOENT"), { code: "ENOENT" });

	test("resolves with stdout when git exits with code 0", async () => {
		assert.strictEqual(
			await runGit(["status"], spawning({ stdout: "ok\n", end: close(0) })),
			"ok\n",
		);
	});

	// git writes fetch progress and "From <remote>" to stderr on success. Logging
	// that at error level puts [ERROR] in the shared output channel on a healthy
	// rules fetch, which is enough to trip the host suite's error monitor.
	test("records stderr below error level when git succeeds", async () => {
		logged.length = 0;
		await runGit(
			["fetch"],
			spawning({ stderr: "From github.com:cwtools/cwtools-hoi4-config\n", end: close(0) }),
		);
		assert.deepStrictEqual(logged, [
			{ level: "warn", message: "git stderr: From github.com:cwtools/cwtools-hoi4-config" },
		]);
	});

	test("rejects and logs stderr at error level when git exits non-zero", async () => {
		logged.length = 0;
		await assert.rejects(
			() => runGit(["pull"], spawning({ stderr: "fatal: not a repo\n", end: close(128) })),
			/git exited with code 128/,
		);
		assert.deepStrictEqual(logged, [
			{ level: "error", message: "git stderr: fatal: not a repo" },
		]);
	});

	test("rejects when git is killed by a signal", async () => {
		await assert.rejects(
			() => runGit(["fetch"], spawning({ end: close(null, "SIGTERM") })),
			/signal: SIGTERM/,
		);
	});

	// Node can emit close after error for some spawn failures; the settled guard
	// must keep the GitNotFoundError rejection, not double-settle.
	test("a missing git rejects with GitNotFoundError, even when close follows", async () => {
		await assert.rejects(
			() =>
				runGit(
					["fetch"],
					spawning({ end: () => [["error", enoent()], ["close", 0, null]] }),
				),
			(err) => {
				assert.ok(err instanceof GitNotFoundError);
				assert.match(err.message, /git was not found on your PATH/);
				return true;
			},
		);
	});

	test("rejects with the raw error when a non-ENOENT spawn error fires", async () => {
		const spawnError = new Error("EACCES");
		await assert.rejects(
			() => runGit(["clone"], spawning({ end: () => [["error", spawnError]] })),
			(err) => err === spawnError,
		);
	});

	test("times out and kills the child when git hangs", async () => {
		let killed = false;
		await assert.rejects(
			() => runGit(["fetch"], spawning({ kill: () => (killed = true) }), 10),
			/timed out/,
		);
		assert.strictEqual(killed, true);
	});
});

suite("engine — rulesFetchCommands", () => {
	const pin = {
		repo: "https://github.com/cwtools/cwtools-hoi4-config",
		ref: "ab1fda2a599ab4318d6f24ecba380e579e37006a",
	};
	const fetchAndCheckout = [
		["-C", "/cache/hoi4", "fetch", "--depth", "1", pin.repo, pin.ref],
		["-C", "/cache/hoi4", "checkout", "--detach", pin.ref],
	];

	// The whole point of the pin: no command may name a branch or a remote HEAD.
	test("builds an empty repo, then fetches and checks out the pin, never a branch tip", () => {
		const commands = rulesFetchCommands("/cache/hoi4", pin, null);
		assert.deepStrictEqual(commands, [
			["init", "--quiet", "/cache/hoi4"],
			...fetchAndCheckout,
		]);
		assert.doesNotMatch(commands.flat().join(" "), /\b(HEAD|master|main|clone|pull)\b/);
	});

	test("moves an existing cache to a new pin without re-initing", () => {
		assert.deepStrictEqual(
			rulesFetchCommands("/cache/hoi4", pin, "f1460d139036e75a8dd065dbd27b27415172654f"),
			fetchAndCheckout,
		);
	});

	test("does nothing when the cache already holds the pin", () => {
		assert.deepStrictEqual(rulesFetchCommands("/cache/hoi4", pin, pin.ref), []);
	});

	test.each(["ref", "repo"] as const)("rejects a %s that begins with -", (field) => {
		assert.throws(
			() => rulesFetchCommands("/cache/hoi4", { ...pin, [field]: "--upload-pack=evil" }, null),
			/must not begin with '-'/,
		);
	});
});
