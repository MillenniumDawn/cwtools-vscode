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

// Each served root starts its own walk. Prefer the deepest root when the rules
// directory is inside the workspace; outside known roots keep the whole-path check.
export function createWatchedPathExcluder(
	roots: string | readonly string[],
): (fsPath: string) => boolean {
	const rootSegments = (typeof roots === "string" ? [roots] : roots).map((root) => {
		const parts = root.toLowerCase().split(/[\\/]/);
		while (parts.length > 1 && parts[parts.length - 1] === "") parts.pop();
		return parts;
	}).sort((a, b) => b.length - a.length);
	return (fsPath) => {
		const segments = fsPath.split(/[\\/]/);
		const fileName = segments.pop() ?? "";
		if (EXCLUDED_FILE_NAMES.includes(fileName) || fileName.endsWith(".md")) {
			return true;
		}
		const matchingRoot = rootSegments.find((root) =>
			segments.length >= root.length && root.every((segment, i) => segments[i].toLowerCase() === segment),
		);
		const first = matchingRoot?.length ?? 0;
		return segments.some(
			(segment, i) =>
				i >= first && EXCLUDED_DIRS.includes(segment.toLowerCase()),
		);
	};
}
