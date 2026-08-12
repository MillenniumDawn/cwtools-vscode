import { suite, test } from "vitest";
import * as assert from "node:assert";
import { resolveVersionFrom } from "./build";

// The tag has to match v*, or the Release workflow never fires and the version
// ships as a local one-platform vsix instead. These pin the derivation from
// both sources: the pushed tag in CI, the CHANGELOG heading everywhere else.

const changelog = `### Unreleased

* Work in progress.

### 2.5.0

* Added the widget.
`;

suite("resolveVersionFrom", () => {
	test("prefixes the changelog fallback with v", () => {
		assert.deepStrictEqual(resolveVersionFrom({}, changelog), {
			version: "2.5.0",
			tag: "v2.5.0",
			preRelease: false,
		});
	});

	test("ignores GITHUB_REF_NAME when TAG_RELEASE is unset", () => {
		const env = { GITHUB_REF_NAME: "refs/heads/main" };
		assert.strictEqual(resolveVersionFrom(env, changelog).tag, "v2.5.0");
	});

	test("passes the pushed tag through on a tag release", () => {
		for (const flag of ["true", "TRUE", "1"]) {
			assert.deepStrictEqual(
				resolveVersionFrom(
					{ TAG_RELEASE: flag, GITHUB_REF_NAME: "v3.1.0" },
					changelog,
				),
				{ version: "3.1.0", tag: "v3.1.0", preRelease: false },
			);
		}
	});

	test("treats any other TAG_RELEASE value as not a tag release", () => {
		for (const flag of ["false", "0", ""]) {
			assert.strictEqual(
				resolveVersionFrom(
					{ TAG_RELEASE: flag, GITHUB_REF_NAME: "v3.1.0" },
					changelog,
				).tag,
				"v2.5.0",
			);
		}
	});

	test("falls back to the changelog when the ref name is blank", () => {
		const env = { TAG_RELEASE: "true", GITHUB_REF_NAME: "  " };
		assert.strictEqual(resolveVersionFrom(env, changelog).tag, "v2.5.0");
	});

	test("flags a prerelease from either source", () => {
		const env = { TAG_RELEASE: "true", GITHUB_REF_NAME: "v1.0.0-beta.2" };
		assert.deepStrictEqual(resolveVersionFrom(env, changelog), {
			version: "1.0.0-beta.2",
			tag: "v1.0.0-beta.2",
			preRelease: true,
		});
		assert.deepStrictEqual(
			resolveVersionFrom({}, "## [1.0.0-beta.2]\n\n* X.\n"),
			{ version: "1.0.0-beta.2", tag: "v1.0.0-beta.2", preRelease: true },
		);
	});

	test("does not double the v on a changelog heading that carries one", () => {
		assert.strictEqual(
			resolveVersionFrom({}, "## v0.9.0\n\n* X.\n").tag,
			"v0.9.0",
		);
	});

	test("throws when the changelog has no version heading", () => {
		assert.throws(
			() => resolveVersionFrom({}, "# Title\n\nBody only.\n"),
			/could not find a version heading/,
		);
	});
});
