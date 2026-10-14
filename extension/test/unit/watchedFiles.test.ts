import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";
import { suite, test } from "vitest";
import {
	CWT_WATCH_GLOB,
	LOCALISATION_WATCH_GLOB,
	SCRIPT_WATCH_GLOB,
	createWatchedPathExcluder,
	forwardWatchedFileEvent,
} from "../../src/host/watchedFiles";

// Mirrors cwtools_file_manager's exclude_patterns and EXCLUDED_DIRS, so the
// cases below are the engine's rules: file names are case-sensitive, directory
// names are not, both match whole segments, and directories count only below
// the workspace root the server walks from.
type Case = [path: string, excluded: boolean];

const POSIX_ROOT = "/mod";
const POSIX_CASES: Case[] = [
	["/mod/Changelog.txt", true],
	["/mod/README.txt", true],
	["/mod/LICENSE.txt", true],
	["/mod/docs/notes.md", true],
	["/mod/dist/bundle.js.map", true],
	["/mod/target/x.txt", true],
	["/mod/.git/hooks/x.txt", true],
	["/mod/node_modules/pkg/common/x.txt", true],
	["/mod/.claude/worktrees/a/common/x.txt", true],
	["/mod/OUT/gfx/x.gfx", true],
	["/mod/common/ideas/x.txt", false],
	["/mod/portraits/leaders/x.txt", false],
	// Whole segments only: a directory merely starting with an excluded name,
	// or a file merely ending with one, still counts.
	["/mod/history/dist_x/a.txt", false],
	["/mod/common/outposts/a.txt", false],
	["/mod/events/my_readme.txt", false],
	// The engine's file-name match is case-sensitive; its directory match isn't.
	["/mod/changelog.txt", false],
];

const WINDOWS_ROOT = "C:\\mod";
const WINDOWS_CASES: Case[] = [
	["C:\\mod\\Changelog.txt", true],
	["C:\\mod\\target\\debug\\x.txt", true],
	["C:\\mod\\.git\\x.txt", true],
	["C:\\mod\\common\\ideas\\x.txt", false],
	["C:\\mod\\history\\dist_x\\a.txt", false],
	// VS Code lowercases the drive letter, and Windows ignores path case.
	["c:\\MOD\\target\\x.txt", true],
	["c:\\MOD\\common\\ideas\\x.txt", false],
	// Forward slashes appear in file URIs on Windows too.
	["C:/mod/dist/x.txt", true],
	["C:/mod/common/ideas/x.txt", false],
];

// A mod checked out under a directory the engine would skip. The walk starts
// at the root, so these ancestors never exclude anything.
const NESTED_CASES: [root: string, path: string, excluded: boolean][] = [
	["/home/u/target/mod", "/home/u/target/mod/common/ideas/a.txt", false],
	["/home/u/out/mod", "/home/u/out/mod/events/a.txt", false],
	["/srv/dist/mod", "/srv/dist/mod/common/a.txt", false],
	[
		"/home/u/.claude/worktrees/mod",
		"/home/u/.claude/worktrees/mod/common/ideas/a.txt",
		false,
	],
	["/home/u/target/mod", "/home/u/target/mod/target/x.txt", true],
	["/home/u/target/mod", "/home/u/target/mod/.git/x.txt", true],
	[
		"/home/u/.claude/worktrees/mod",
		"/home/u/.claude/worktrees/mod/node_modules/p/x.txt",
		true,
	],
	["/home/u/target/mod", "/home/u/target/mod/Changelog.txt", true],
	["C:\\build\\target\\mod", "C:\\build\\target\\mod\\common\\a.txt", false],
	["c:\\build\\target\\mod", "C:\\Build\\Target\\Mod\\common\\a.txt", false],
	[
		"C:\\u\\.claude\\worktrees\\mod",
		"C:\\u\\.claude\\worktrees\\mod\\events\\a.txt",
		false,
	],
	["C:\\build\\target\\mod", "C:\\build\\target\\mod\\bin\\a.txt", true],
	["C:\\build\\target\\mod", "C:\\build\\target\\mod\\README.txt", true],
	// A trailing separator on the root changes nothing.
	["/home/u/target/mod/", "/home/u/target/mod/common/a.txt", false],
	["C:\\build\\target\\mod\\", "C:\\build\\target\\mod\\obj\\a.txt", true],
	// A drive or filesystem root as the workspace.
	["/", "/target/x.txt", true],
	["/", "/common/x.txt", false],
	["C:\\", "C:\\common\\x.txt", false],
	["C:\\", "C:\\obj\\x.txt", true],
];

// Outside the root, every directory segment counts, as the whole-path check did
// before the root was known.
const OUTSIDE_CASES: [root: string, path: string, excluded: boolean][] = [
	["/mod", "/other/dist/x.txt", true],
	["/mod", "/other/.claude/rules/x.cwt", true],
	// A sibling that merely shares the root's name as a prefix is outside it.
	["/mod", "/mod2/target/x.txt", true],
	["C:\\mod", "D:\\mod\\target\\x.txt", true],
	["C:\\mod", "C:\\other\\dist\\x.txt", true],
	["/mod", "/other/Changelog.txt", true],
	["/mod", "/other/docs/notes.md", true],
	["C:\\mod", "C:\\other\\README.txt", true],
	["/mod", "/other/common/ideas/x.txt", false],
	["/mod", "/x.txt", false],
];

// The file classes the server indexes, read from the engine source so the
// globs are checked against it and not against a copy of their own list.
function engineFileClasses() {
	const crates = path.resolve(__dirname, "../../../engine/crates");
	const read = (file: string) => fs.readFileSync(path.join(crates, file), "utf8");
	const quoted = (text: string) => [...text.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
	const fileManager = read("file_manager/src/file_manager.rs");
	const lspPaths = read("lsp/src/paths.rs");
	const script = /pub const SCRIPT_EXTENSIONS: &\[&str\] = &\[([^\]]*)\]/.exec(fileManager);
	const locExtensions = /pub fn is_loc_ext\(ext: &str\) -> bool \{([^}]*)\}/.exec(fileManager);
	const locDirs = /const LOC_DIR_NAMES: \[&str; \d+\] = \[([^\]]*)\]/.exec(lspPaths);
	const cwt = /fn is_cwt_file[^}]*ends_with\("\.(\w+)"\)/.exec(lspPaths);
	assert.ok(
		script && locExtensions && locDirs && cwt,
		"the engine's file-class definitions moved; update this parser",
	);
	return {
		script: quoted(script[1]).sort(),
		locExtensions: quoted(locExtensions[1]).sort(),
		locDirs: quoted(locDirs[1]).sort(),
		cwt: cwt[1],
	};
}

suite("watchedFiles", () => {
	test("watches every file class the server indexes", () => {
		const engine = engineFileClasses();
		const sorted = (list: string) => list.split(",").sort();
		const script = /^\*\*\/\*\.\{([^}]+)\}$/.exec(SCRIPT_WATCH_GLOB);
		assert.ok(script, SCRIPT_WATCH_GLOB);
		assert.deepStrictEqual(sorted(script[1]), engine.script);
		const loc = /^\*\*\/\{([^}]+)\}\/\*\*\/\*\.\{([^}]+)\}$/.exec(LOCALISATION_WATCH_GLOB);
		assert.ok(loc, LOCALISATION_WATCH_GLOB);
		assert.deepStrictEqual(sorted(loc[1]), engine.locDirs);
		assert.deepStrictEqual(sorted(loc[2]), engine.locExtensions);
		assert.strictEqual(CWT_WATCH_GLOB, `**/*.${engine.cwt}`);
	});

	test("drops what the server's discovery walk would skip", () => {
		const excluded = createWatchedPathExcluder(POSIX_ROOT);
		for (const [path, expected] of POSIX_CASES) {
			assert.strictEqual(excluded(path), expected, path);
		}
	});

	test("applies the same rules to Windows paths", () => {
		const excluded = createWatchedPathExcluder(WINDOWS_ROOT);
		for (const [path, expected] of WINDOWS_CASES) {
			assert.strictEqual(excluded(path), expected, path);
		}
	});

	test("counts directories below the root only, not the root's ancestors", () => {
		for (const [root, path, expected] of NESTED_CASES) {
			assert.strictEqual(
				createWatchedPathExcluder(root)(path),
				expected,
				`${root} :: ${path}`,
			);
		}
	});

	test("counts every directory of a path outside the root", () => {
		for (const [root, path, expected] of OUTSIDE_CASES) {
			assert.strictEqual(
				createWatchedPathExcluder(root)(path),
				expected,
				`${root} :: ${path}`,
			);
		}
	});

	test("measures only .cwt files from the rules root", () => {
		const cases: [rulesRoot: string, path: string, excluded: boolean][] = [
			["/mod/.claude/rules", "/mod/.claude/rules/a.cwt", false],
			["/mod/.claude/rules", "/mod/.claude/rules/target/a.cwt", true],
			["/mod/.claude/rules", "/mod/.claude/rules/a.txt", true],
			["/mod/.claude/rules", "/mod/.claude/rules/localisation/a.yml", true],
			["/other/dist/rules", "/other/dist/rules/a.cwt", false],
			["/other/dist/rules", "/other/dist/rules/a.txt", true],
			["C:\\mod\\.claude\\rules", "c:\\mod\\.claude\\rules\\A.CWT", false],
			["C:\\mod\\.claude\\rules", "c:\\mod\\.claude\\rules\\a.txt", true],
		];
		for (const [rulesRoot, path, expected] of cases) {
			const root = rulesRoot.startsWith("C:") ? WINDOWS_ROOT : POSIX_ROOT;
			assert.strictEqual(
				createWatchedPathExcluder([root], rulesRoot)(path),
				expected,
				`${rulesRoot} :: ${path}`,
			);
		}
	});

	test("filters excluded watched-file events before forwarding to the server", async () => {
		const excluded = createWatchedPathExcluder(POSIX_ROOT);
		const checkedPaths: string[] = [];
		const forwardedUris: string[] = [];
		const isExcluded = (fsPath: string) => {
			checkedPaths.push(fsPath);
			return excluded(fsPath);
		};
		const fileUriToPath = (uri: string) => new URL(uri).pathname;
		const next = (event: { uri: string }) => {
			forwardedUris.push(event.uri);
			return Promise.resolve();
		};

		const excludedEvent = { uri: "file:///mod/target/probe.txt" };
		await forwardWatchedFileEvent(excludedEvent, isExcluded, fileUriToPath, next);
		assert.deepStrictEqual(checkedPaths, ["/mod/target/probe.txt"]);
		assert.deepStrictEqual(forwardedUris, [], "excluded event must not reach the server");

		const includedEvent = { uri: "file:///mod/common/probe.txt" };
		await forwardWatchedFileEvent(includedEvent, isExcluded, fileUriToPath, next);
		assert.deepStrictEqual(checkedPaths, ["/mod/target/probe.txt", "/mod/common/probe.txt"]);
		assert.deepStrictEqual(forwardedUris, [includedEvent.uri]);
	});
});
