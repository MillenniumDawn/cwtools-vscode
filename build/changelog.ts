// Pure CHANGELOG.md parsing shared by the release commands. Reading the file
// and calling gh stay in build.ts; these functions only turn text into values
// so the heading logic is unit-testable.

// "## [x.y.z]", "## x.y.z", "### v1.0.0-beta.2" -- capture group is the version.
const headingRe = /^#+\s*\[?v?(\d+\.\d+\.\d+[^\]\s]*)\]?/m;

// First version heading in the changelog text. Non-version headings such as
// "### Unreleased" are skipped.
export function topChangelogVersion(changelog: string): string {
	const m = changelog.match(headingRe);
	if (!m) throw new Error('could not find a version heading in CHANGELOG.md');
	return m[1];
}

// The CHANGELOG section body for `version`, used as the GitHub release notes.
// Throws when there is no such section, so a tag can't silently ship generic
// auto-generated notes.
export function releaseNotes(changelog: string, version: string): string {
	const lines = changelog.split('\n');
	const start = lines.findIndex((l) => l.match(headingRe)?.[1] === version);
	const rest = start === -1 ? [] : lines.slice(start + 1);
	const end = rest.findIndex((l) => headingRe.test(l));
	const notes = (end === -1 ? rest : rest.slice(0, end)).join('\n').trim();
	if (!notes) {
		throw new Error(
			`no CHANGELOG section for version ${version}; refusing to publish a release with auto-generated notes`,
		);
	}
	return notes;
}
