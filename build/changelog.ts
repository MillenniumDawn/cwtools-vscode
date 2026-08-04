// Pure CHANGELOG.md parsing shared by the release commands. Reading the file
// and calling gh stay in build.ts; these functions only turn text into values
// so the heading logic is unit-testable.

// First "## [x.y.z]" (or "## x.y.z") heading in the changelog text.
export function topChangelogVersion(changelog: string): string {
	const m = changelog.match(/^#+\s*\[?v?(\d+\.\d+\.\d+[^\]\s]*)\]?/m);
	if (!m) throw new Error("could not find a version heading in CHANGELOG.md");
	return m[1];
}

// The CHANGELOG section body for `version`, used as the GitHub release notes.
// Returns '' when no heading matches (the caller decides how to fail).
export function changelogNotes(changelog: string, version: string): string {
	const lines = changelog.split("\n");
	const headingRe = /^#+\s*\[?v?(\d+\.\d+\.\d+[^\]\s]*)\]?/;
	let start = -1;
	for (let i = 0; i < lines.length; i++) {
		const m = lines[i].match(headingRe);
		if (m && m[1] === version) {
			start = i + 1;
			break;
		}
	}
	if (start === -1) return "";
	const body: string[] = [];
	for (let i = start; i < lines.length; i++) {
		if (headingRe.test(lines[i])) break;
		body.push(lines[i]);
	}
	return body.join("\n").trim();
}

// The release notes for `version`, failing when the CHANGELOG has no matching
// section so a tag can't silently ship auto-generated notes.
export function releaseNotes(changelog: string, version: string): string {
	const notes = changelogNotes(changelog, version);
	if (!notes) {
		throw new Error(
			`no CHANGELOG section for version ${version}; refusing to publish a release with auto-generated notes`,
		);
	}
	return notes;
}
