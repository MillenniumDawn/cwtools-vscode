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

// Keep filtering separate from VS Code's FileSystemWatcher so it can be
// exercised with synthetic file events even when the host suppresses events
// from ignored directories such as target/.
export async function forwardWatchedFileEvent<T extends { uri: string }>(
	event: T,
	isExcluded: (fsPath: string) => boolean,
	fileUriToPath: (uri: string) => string,
	next: (event: T) => Promise<void>,
): Promise<void> {
	if (isExcluded(fileUriToPath(event.uri))) return;
	await next(event);
}

// Each served root starts its own walk, so the deepest matching root wins. The
// rules root counts for .cwt files only: other files below it are workspace
// script. Outside known roots keep the whole-path check.
export function createWatchedPathExcluder(
	roots: string | readonly string[],
	rulesRoot?: string,
): (fsPath: string) => boolean {
	const toSegments = (root: string) => {
		const parts = root.toLowerCase().split(/[\\/]/);
		while (parts.length > 1 && parts[parts.length - 1] === "") parts.pop();
		return parts;
	};
	const deepestFirst = (a: string[], b: string[]) => b.length - a.length;
	const scriptRoots = (typeof roots === "string" ? [roots] : roots)
		.map(toSegments)
		.sort(deepestFirst);
	const cwtRoots =
		rulesRoot === undefined
			? scriptRoots
			: [...scriptRoots, toSegments(rulesRoot)].sort(deepestFirst);
	return (fsPath) => {
		const segments = fsPath.split(/[\\/]/);
		const fileName = segments.pop() ?? "";
		if (EXCLUDED_FILE_NAMES.includes(fileName) || fileName.endsWith(".md")) {
			return true;
		}
		const rootSegments = fileName.toLowerCase().endsWith(".cwt")
			? cwtRoots
			: scriptRoots;
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
