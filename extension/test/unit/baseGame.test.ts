import * as assert from "assert";
import { readFileSync } from "fs";
import { suite, test } from "vitest";
import { GAMES } from "../../src/host/games";
import {
	findVanillaInstallations,
	parseSteamLibraryFolders,
	steamRootCandidates,
} from "../../src/host/baseGame";

const fixture = readFileSync(
	"extension/test/fixtures/steam-libraryfolders.vdf",
	"utf8",
);

suite("Steam base game discovery", () => {
	test("reads escaped Windows paths from realistic libraryfolders.vdf", () => {
		assert.deepStrictEqual(parseSteamLibraryFolders(fixture), [
			"C:\\Program Files (x86)\\Steam",
			"D:\\Games\\SteamLibrary",
		]);
	});

	test("finds HOI4 in a secondary Steam library", () => {
		const existing = new Set([
			"D:\\Games\\SteamLibrary\\steamapps\\common\\Hearts of Iron IV",
		]);
		assert.deepStrictEqual(
			findVanillaInstallations(
				GAMES.find((game) => game.id === "hoi4")!,
				[fixture],
				(p) => existing.has(p),
				(directory) =>
					directory === "D:\\Games\\SteamLibrary\\steamapps\\common"
						? ["Other game", "Hearts of Iron IV"]
						: [],
			),
			["D:\\Games\\SteamLibrary\\steamapps\\common\\Hearts of Iron IV"],
		);
	});

	test("appends game subdirectories where the game data is nested", () => {
		const eu5 = GAMES.find((game) => game.id === "eu5")!;
		const expected = "D:\\Games\\SteamLibrary\\steamapps\\common\\Europa Universalis V\\game";
		assert.deepStrictEqual(
			findVanillaInstallations(
				eu5,
				[fixture],
				(candidate) => candidate === expected,
				() => ["Europa Universalis V"],
			),
			[expected],
		);
	});

	test("includes common Steam roots for the selected platform", () => {
		assert.deepStrictEqual(
			steamRootCandidates("win32", "C:\\Users\\tester", {
				LOCALAPPDATA: "C:\\Users\\tester\\AppData\\Local",
				"ProgramFiles(x86)": "C:\\Program Files (x86)",
			}),
			[
				"C:\\Users\\tester\\AppData\\Local\\Programs\\Steam",
				"C:\\Program Files (x86)\\Steam",
			],
		);
	});
});
