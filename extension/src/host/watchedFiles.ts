// The watcher globs match by extension, the same way the server's workspace
// walk does, but that walk also drops free-form text files and tooling
// directories before it reads anything. Its watched-file path applies neither
// (MillenniumDawn/cwtools#314), so mirror both lists here and drop the events
// rather than have a changelog or a build directory validated as game script.

// cwtools_file_manager's FileManagerConfig::default().exclude_patterns, matched
// against the file name as the engine does: case-sensitive, `*.md` as a suffix.
const EXCLUDED_FILE_NAMES = [
	"Changelog.txt",
	"README.txt",
	"LICENSE.txt",
	"README.md",
	"LICENSE.md",
];

// cwtools_file_manager's EXCLUDED_DIRS, matched per whole segment and
// case-insensitively as `is_excluded_dir` does.
const EXCLUDED_DIRS = [
	".git",
	".claude",
	"target",
	".vs",
	"node_modules",
	"out",
	"dist",
	"bin",
	"obj",
	".idea",
	".vscode",
];

// The server's walk starts at the workspace root, so the root's own ancestors
// never exclude anything. A path outside the root keeps the whole-path check.
export function createWatchedPathExcluder(
	root: string,
): (fsPath: string) => boolean {
	const rootSegments = root.toLowerCase().split(/[\\/]/);
	while (
		rootSegments.length > 1 &&
		rootSegments[rootSegments.length - 1] === ""
	) {
		rootSegments.pop();
	}
	return (fsPath) => {
		const segments = fsPath.split(/[\\/]/);
		const fileName = segments.pop() ?? "";
		if (EXCLUDED_FILE_NAMES.includes(fileName) || fileName.endsWith(".md")) {
			return true;
		}
		const underRoot =
			segments.length >= rootSegments.length &&
			rootSegments.every(
				(segment, i) => segments[i].toLowerCase() === segment,
			);
		const first = underRoot ? rootSegments.length : 0;
		return segments.some(
			(segment, i) =>
				i >= first && EXCLUDED_DIRS.includes(segment.toLowerCase()),
		);
	};
}
