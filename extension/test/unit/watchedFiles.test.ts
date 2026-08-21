import * as assert from "assert";
import { suite, test } from "vitest";
import { isExcludedWatchedPath } from "../../src/host/watchedFiles";

// Mirrors cwtools_file_manager's exclude_patterns and EXCLUDED_DIRS, so the
// cases below are the engine's rules: file names are case-sensitive, directory
// names are not, and both match whole segments.
const CASES: [path: string, excluded: boolean][] = [
	["/mod/Changelog.txt", true],
	["/mod/README.txt", true],
	["/mod/LICENSE.txt", true],
	["/mod/docs/notes.md", true],
	["/mod/dist/bundle.js.map", true],
	["/mod/node_modules/pkg/common/x.txt", true],
	["/mod/.claude/worktrees/a/common/x.txt", true],
	["/mod/OUT/gfx/x.gfx", true],
	["C:\\mod\\target\\debug\\x.txt", true],
	["/mod/common/ideas/x.txt", false],
	["/mod/portraits/leaders/x.txt", false],
	// Whole segments only: a directory merely starting with an excluded name,
	// or a file merely ending with one, still counts.
	["/mod/history/dist_x/a.txt", false],
	["/mod/common/outposts/a.txt", false],
	["/mod/events/my_readme.txt", false],
	// The engine's file-name match is case-sensitive; its directory match isn't.
	["/mod/changelog.txt", false],
	["C:\\mod\\common\\ideas\\x.txt", false],
];

suite("watchedFiles", () => {
	test("drops what the server's discovery walk would skip", () => {
		for (const [path, excluded] of CASES) {
			assert.strictEqual(isExcludedWatchedPath(path), excluded, path);
		}
	});
});
