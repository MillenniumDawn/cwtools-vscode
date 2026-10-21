import * as assert from "assert";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { suite, test } from "vitest";
import { GAMES } from "../../src/host/games";
import {
	findVanillaInstallations,
	parseSteamLibraryFolders,
} from "../../src/host/baseGame";

const game = (id: string) => GAMES.find((item) => item.id === id)!;

suite("Steam base game discovery", () => {
	test("reads escaped Windows paths from libraryfolders.vdf", () => {
		const vdf = fs.readFileSync(
			path.join(__dirname, "..", "fixtures", "steam-libraryfolders.vdf"),
			"utf8",
		);
		assert.deepStrictEqual(parseSteamLibraryFolders(vdf), [
			"C:\\Program Files (x86)\\Steam",
			"D:\\Games\\SteamLibrary",
		]);
	});

	test("finds installs in a secondary library and appends the game subfolder", async () => {
		const temp = fs.mkdtempSync(path.join(os.tmpdir(), "cwtools-steam-"));
		try {
			const steam = path.join(temp, "steam");
			const library = path.join(temp, "library");
			const common = path.join(library, "steamapps", "common");
			fs.mkdirSync(path.join(steam, "steamapps"), { recursive: true });
			fs.mkdirSync(path.join(common, "Hearts of Iron IV"), { recursive: true });
			fs.mkdirSync(path.join(common, "Europa Universalis V", "game"), {
				recursive: true,
			});
			fs.mkdirSync(path.join(common, "Crusader Kings III"), { recursive: true });
			fs.writeFileSync(
				path.join(steam, "steamapps", "libraryfolders.vdf"),
				`"libraryfolders"\n{\n\t"0"\n\t{\n\t\t"path"\t\t"${library.replace(/\\/g, "\\\\")}"\n\t}\n}\n`,
			);

			assert.deepStrictEqual(await findVanillaInstallations(game("hoi4"), [steam]), [
				path.join(common, "Hearts of Iron IV"),
			]);
			assert.deepStrictEqual(await findVanillaInstallations(game("eu5"), [steam]), [
				path.join(common, "Europa Universalis V", "game"),
			]);
			assert.deepStrictEqual(await findVanillaInstallations(game("ck3"), [steam]), []);
		} finally {
			fs.rmSync(temp, { recursive: true, force: true });
		}
	});
});
