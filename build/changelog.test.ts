import { suite, test } from 'vitest';
import * as assert from 'node:assert';
import { releaseNotes, topChangelogVersion } from './changelog';

const threeReleases = `### Unreleased

* Work in progress.

### 2.5.0

* Added the widget.
* Fixed the flange.

### 2.4.0

* Old notes.
`;

suite('releaseNotes', () => {
	test('returns the section body for a present version', () => {
		assert.strictEqual(
			releaseNotes(threeReleases, '2.5.0'),
			'* Added the widget.\n* Fixed the flange.',
		);
	});

	test('returns the full tail when the section is the last heading', () => {
		assert.strictEqual(releaseNotes(threeReleases, '2.4.0'), '* Old notes.');
	});

	test('throws for a missing version (the silent --generate-notes trigger)', () => {
		assert.throws(
			() => releaseNotes(threeReleases, '9.9.9'),
			/refusing to publish a release with auto-generated notes/,
		);
	});

	test('does not match a non-version heading', () => {
		assert.throws(() => releaseNotes(threeReleases, 'Unreleased'));
	});

	test('requires an exact version, not a prefix', () => {
		assert.throws(() => releaseNotes(threeReleases, '2.5'));
		assert.throws(() => releaseNotes(threeReleases, '2'));
	});

	test('does not match a prerelease section with the bare version', () => {
		const changelog = '## [1.0.0-beta.2]\n\n* Notes.\n';
		assert.throws(() => releaseNotes(changelog, '1.0.0'));
		assert.strictEqual(releaseNotes(changelog, '1.0.0-beta.2'), '* Notes.');
	});

	test('trims blank lines around the body', () => {
		const changelog = '### 1.0.0\n\n\n* Notes.\n\n### 2.0.0\n\n* Two.\n';
		assert.strictEqual(releaseNotes(changelog, '1.0.0'), '* Notes.');
	});

	test('throws for an empty section body', () => {
		const changelog = '### 1.0.0\n\n### 2.0.0\n\n* Two.\n';
		assert.throws(() => releaseNotes(changelog, '1.0.0'));
	});

	test('throws when the last heading in the file has no body before EOF', () => {
		const changelog = '### 2.0.0\n\n* Two.\n\n### 1.0.0';
		assert.throws(() => releaseNotes(changelog, '1.0.0'));
	});

	test('does not treat an indented heading-like line as a section boundary', () => {
		const changelog =
			'### 1.0.0\n\n* Example:\n  ### 2.5.0 not a real heading\n\n### 0.9.0\n\n* Old.\n';
		assert.strictEqual(
			releaseNotes(changelog, '1.0.0'),
			'* Example:\n  ### 2.5.0 not a real heading',
		);
	});

	test('finds a section whose heading has trailing decoration', () => {
		const changelog = '### 2.5.0 - 2026-08-01\n\n* X.\n\n### 2.4.0\n\n* Y.\n';
		assert.strictEqual(releaseNotes(changelog, '2.5.0'), '* X.');
	});
});

suite('topChangelogVersion', () => {
	test('returns the first version heading, skipping non-version headings', () => {
		assert.strictEqual(topChangelogVersion(threeReleases), '2.5.0');
	});

	test('handles bracketed, v-prefixed, and prerelease headings', () => {
		assert.strictEqual(topChangelogVersion('## [1.0.0]\n\n* X.\n'), '1.0.0');
		assert.strictEqual(topChangelogVersion('## v0.9.0\n\n* X.\n'), '0.9.0');
		assert.strictEqual(
			topChangelogVersion('## [1.0.0-beta.2]\n\n* X.\n'),
			'1.0.0-beta.2',
		);
	});

	test('ignores a heading that is not at the start of a line', () => {
		const changelog = 'text ### 1.0.0\n\n### 2.0.0\n\n* Two.\n';
		assert.strictEqual(topChangelogVersion(changelog), '2.0.0');
	});

	test('tolerates trailing text after the version on the heading line', () => {
		assert.strictEqual(topChangelogVersion('### 2.5.0 - 2026-08-01\n\n* X.\n'), '2.5.0');
	});

	test('throws when there is no version heading', () => {
		assert.throws(
			() => topChangelogVersion('# Title\n\nBody only.\n'),
			/could not find a version heading/,
		);
	});
});
