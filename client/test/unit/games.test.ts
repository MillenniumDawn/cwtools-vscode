import { suite, test } from "vitest";
import * as assert from "assert";
import {
	GAMES,
	GAME_IDS,
	LANGUAGE_REPOS,
	GAME_DISPLAY,
	GAME_FOLDER,
	FOLDER_HINTS,
	CONTENT_HINTS,
} from "../../extension/games";
import { detectFromFolder } from "../../extension/engine";

suite("games — table shape", () => {
	test("every game has the required fields", () => {
		for (const g of GAMES) {
			assert.ok(g.id, "id");
			assert.ok(g.display, `${g.id} display`);
			assert.match(g.repo, /^https:\/\/github\.com\//, `${g.id} repo`);
			assert.ok(g.exeName, `${g.id} exeName`);
			assert.ok(g.vanillaFolders.length > 0, `${g.id} vanillaFolders`);
			assert.ok(g.folderHint, `${g.id} folderHint`);
		}
	});

	test("ids are unique", () => {
		assert.strictEqual(new Set(GAME_IDS).size, GAMES.length);
	});
});

suite("games — derived maps match the pre-consolidation literals", () => {
	test("LANGUAGE_REPOS", () => {
		assert.deepStrictEqual(LANGUAGE_REPOS, {
			stellaris: "https://github.com/cwtools/cwtools-stellaris-config",
			eu4: "https://github.com/cwtools/cwtools-eu4-config",
			hoi4: "https://github.com/cwtools/cwtools-hoi4-config",
			ck2: "https://github.com/cwtools/cwtools-ck2-config",
			imperator: "https://github.com/cwtools/cwtools-ir-config",
			vic2: "https://github.com/cwtools/cwtools-vic2-config",
			vic3: "https://github.com/cwtools/cwtools-vic3-config",
			ck3: "https://github.com/cwtools/cwtools-ck3-config",
			eu5: "https://github.com/kaiser-chris/cwtools-eu5-config",
		});
	});

	test("GAME_DISPLAY", () => {
		assert.deepStrictEqual(GAME_DISPLAY, {
			stellaris: "Stellaris",
			hoi4: "Hearts of Iron IV",
			eu4: "Europa Universalis IV",
			ck2: "Crusader Kings II",
			imperator: "Imperator",
			vic2: "Victoria II",
			vic3: "Victoria 3",
			ck3: "Crusader Kings III",
			eu5: "Europa Universalis V",
		});
	});

	test("GAME_FOLDER", () => {
		assert.deepStrictEqual(GAME_FOLDER, {
			"stellaris": { id: "stellaris" },
			"hearts of iron iv": { id: "hoi4" },
			"europa universalis iv": { id: "eu4" },
			"crusader kings ii": { id: "ck2" },
			"crusader kings iii": { id: "ck3", subdir: "game" },
			"victoria ii": { id: "vic2" },
			"victoria 2": { id: "vic2" },
			"victoria 3": { id: "vic3", subdir: "game" },
			"imperatorrome": { id: "imperator", subdir: "game" },
			"imperator": { id: "imperator", subdir: "game" },
			"europa universalis v": { id: "eu5", subdir: "game" },
		});
	});

	test("CONTENT_HINTS covers the same markers", () => {
		assert.deepStrictEqual(new Map(CONTENT_HINTS), new Map([
			["common/ai_strategy", "hoi4"],
			["common/species_classes", "stellaris"],
			["common/great_projects", "eu4"],
			["common/dynasties", "ck3"],
		]));
	});

	test("FOLDER_HINTS has one hint per game", () => {
		assert.deepStrictEqual(FOLDER_HINTS.map(([, id]) => id), GAME_IDS);
	});
});

suite("games — hint ordering fix", () => {
	const noopExists = () => false;

	test("3-suffixed games are ordered before their 2-suffixed prefixes", () => {
		const order = GAME_IDS;
		assert.ok(order.indexOf("ck3") < order.indexOf("ck2"));
		assert.ok(order.indexOf("vic3") < order.indexOf("vic2"));
	});

	test("a Crusader Kings III folder detects as ck3, not ck2", () => {
		assert.strictEqual(detectFromFolder("/x/Crusader Kings III", noopExists), "ck3");
	});

	test("a Victoria III folder detects as vic3, not vic2", () => {
		assert.strictEqual(detectFromFolder("/x/Victoria III", noopExists), "vic3");
	});

	test("ck2 and vic2 folders still detect correctly", () => {
		assert.strictEqual(detectFromFolder("/x/Crusader Kings II", noopExists), "ck2");
		assert.strictEqual(detectFromFolder("/x/Victoria II", noopExists), "vic2");
	});
});
