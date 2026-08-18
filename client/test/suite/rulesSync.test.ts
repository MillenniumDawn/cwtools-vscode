import * as assert from "assert";
import * as path from "path";
import * as fsPromises from "fs/promises";
import { runGit } from "../../extension/engine";
import { activate, waitUntil } from "../support/utils";

// Fixed commits in the checked-in bare repo at
// client/test/fixtures/hoi4-rules.git (see .vscode-test.mjs, which points
// CWTOOLS_TEST_HOI4_REPO/_REF at it for this label). NEW_PIN is the commit
// games.ts is pointed at for this run; OLD_PIN exists only so the shallow
// --depth 1 fetch has something to prove it left behind.
const OLD_PIN = "7ac77548398c279ef48fff6476c71ec622c3073f";
const NEW_PIN = "3f03757a6f15565f763434e5752021c3ba8c0c3e";
const MARKER_PATH = path.join("common", "ai_strategy", "hoi4_test_rules.cwt");

suite("Rules sync — hoi4 fixture", function () {
	this.timeout(30_000);

	test("clones the pinned commit from the local fixture repo with zero network", async () => {
		const api = await activate();
		assert.ok(api, "extension should expose its activation API");
		const cacheRoot = api.rulesCacheRoot();
		assert.ok(cacheRoot, "activation should have resolved a rules cache root");
		const hoi4Cache = path.join(cacheRoot, "hoi4");
		const markerPath = path.join(hoi4Cache, MARKER_PATH);

		const populated = await waitUntil(async () => {
			try {
				const text = await fsPromises.readFile(markerPath, "utf8");
				return text.includes("marker = new_pin");
			} catch {
				return false;
			}
		}, 15_000);
		assert.ok(
			populated,
			`rules cache should be populated with the pinned fixture content at ${markerPath}`,
		);

		const head = (
			await runGit(["-C", hoi4Cache, "rev-parse", "HEAD"])
		).trim();
		assert.strictEqual(head, NEW_PIN, "cache should land on the pinned commit");

		await assert.rejects(
			runGit(["-C", hoi4Cache, "cat-file", "-e", OLD_PIN]),
			"the shallow --depth 1 fetch should not have pulled in the previous commit",
		);
	});
});
