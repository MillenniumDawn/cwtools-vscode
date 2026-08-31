import { suite, test } from "vitest";
import * as assert from "assert";
import {
	GAMES,
	LANGUAGE_REPOS,
	FOLDER_HINTS,
	CONTENT_HINTS,
} from "../../src/host/games";
import { detectFromFolder } from "../../src/host/engine";

suite("games — table shape", () => {
	test("every game has the required fields", () => {
		for (const g of GAMES) {
			assert.ok(g.id, "id");
			assert.ok(g.display, `${g.id} display`);
			assert.match(g.repo, /^https:\/\/github\.com\//, `${g.id} repo`);
			assert.match(g.repoRef, /^[0-9a-f]{40}$/, `${g.id} repoRef`);
			assert.ok(g.exeName, `${g.id} exeName`);
			assert.ok(g.vanillaFolders.length > 0, `${g.id} vanillaFolders`);
			assert.ok(g.folderHint, `${g.id} folderHint`);
		}
	});

	test("ids are unique", () => {
		assert.strictEqual(new Set(GAMES.map((g) => g.id)).size, GAMES.length);
	});
});

suite("games — derived maps match the pre-consolidation literals", () => {
	test("LANGUAGE_REPOS", () => {
		assert.deepStrictEqual(
			Object.fromEntries(
				Object.entries(LANGUAGE_REPOS).map(([id, r]) => [id, r.repo]),
			),
			{
				stellaris: "https://github.com/cwtools/cwtools-stellaris-config",
				eu4: "https://github.com/cwtools/cwtools-eu4-config",
				hoi4: "https://github.com/cwtools/cwtools-hoi4-config",
				ck2: "https://github.com/cwtools/cwtools-ck2-config",
				imperator: "https://github.com/cwtools/cwtools-ir-config",
				vic2: "https://github.com/cwtools/cwtools-vic2-config",
				vic3: "https://github.com/cwtools/cwtools-vic3-config",
				ck3: "https://github.com/cwtools/cwtools-ck3-config",
				eu5: "https://github.com/kaiser-chris/cwtools-eu5-config",
			},
		);
	});

	test("LANGUAGE_REPOS carries each game's pin", () => {
		for (const g of GAMES) {
			assert.strictEqual(LANGUAGE_REPOS[g.id].ref, g.repoRef, `${g.id} ref`);
		}
	});

	test("CONTENT_HINTS covers the same markers", () => {
		assert.deepStrictEqual(
			new Map(CONTENT_HINTS),
			new Map([
				["common/ai_strategy", "hoi4"],
				["common/species_classes", "stellaris"],
				["common/great_projects", "eu4"],
				["common/dynasties", "ck3"],
			]),
		);
	});

	test("FOLDER_HINTS has one hint per game", () => {
		assert.deepStrictEqual(
			FOLDER_HINTS.map(([, id]) => id),
			GAMES.map((g) => g.id),
		);
	});
});

suite("games — hint ordering fix", () => {
	const noopExists = () => false;

	test("newer games are ordered before their older prefixes", () => {
		const order = GAMES.map((g) => g.id);
		assert.ok(order.indexOf("ck3") < order.indexOf("ck2"));
		assert.ok(order.indexOf("vic3") < order.indexOf("vic2"));
		assert.ok(order.indexOf("eu5") < order.indexOf("eu4"));
	});

	test("a Crusader Kings III folder detects as ck3, not ck2", async () => {
		assert.strictEqual(
			await detectFromFolder("/x/Crusader Kings III", noopExists),
			"ck3",
		);
	});

	test("a Victoria III folder detects as vic3, not vic2", async () => {
		assert.strictEqual(
			await detectFromFolder("/x/Victoria III", noopExists),
			"vic3",
		);
	});

	test("an Europa Universalis V folder detects as eu5, not eu4", async () => {
		assert.strictEqual(
			await detectFromFolder(
				"/x/Documents/Paradox Interactive/Europa Universalis V/mod/m",
				noopExists,
			),
			"eu5",
		);
	});

	test("ck2 and vic2 folders still detect correctly", async () => {
		assert.strictEqual(
			await detectFromFolder("/x/Crusader Kings II", noopExists),
			"ck2",
		);
		assert.strictEqual(
			await detectFromFolder("/x/Victoria II", noopExists),
			"vic2",
		);
	});
});
