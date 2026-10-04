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
// case-insensitively as `is_excluded_dir` does. The walk starts at the workspace
// root and only checks what lies below it, so a mod checked out under a
// directory with one of these names is still served.
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

// Directory names count only below `root`. A path outside it, such as a file
// from another folder of a multi-root workspace, gets the file-name check alone
// and the server's own access boundary decides the rest. Both separators are
// split and the root is compared case-insensitively, so Windows paths work
// wherever this runs.
export function createWatchedPathExcluder(
	root: string,
): (fsPath: string) => boolean {
	const rootSegments = root.split(/[\\/]/);
	while (rootSegments.length > 1 && rootSegments[rootSegments.length - 1] === "") {
		rootSegments.pop();
	}
	const rootPrefix = rootSegments.map((segment) => segment.toLowerCase());
	return (fsPath) => {
		const segments = fsPath.split(/[\\/]/);
		const fileName = segments.pop() ?? "";
		if (EXCLUDED_FILE_NAMES.includes(fileName) || fileName.endsWith(".md")) {
			return true;
		}
		if (segments.length < rootPrefix.length) {
			return false;
		}
		for (let i = 0; i < rootPrefix.length; i++) {
			if (segments[i].toLowerCase() !== rootPrefix[i]) {
				return false;
			}
		}
		for (let i = rootPrefix.length; i < segments.length; i++) {
			if (EXCLUDED_DIRS.includes(segments[i].toLowerCase())) {
				return true;
			}
		}
		return false;
	};
}
