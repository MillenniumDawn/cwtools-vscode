import * as os from "os";
import * as path from "path";
import { access, readFile, readdir } from "fs/promises";
import type { GameDef } from "./games";

// Steam writes each library as a quoted `"path"` line with `\\` and `\"` escapes.
const LIBRARY_PATH = /^\s*"path"\s+"((?:[^"\\]|\\.)*)"/gm;

function steamRoots(): string[] {
	const home = os.homedir();
	switch (process.platform) {
		case "win32":
			return ["C:\\Program Files (x86)\\Steam", "C:\\Program Files\\Steam"];
		case "darwin":
			return [path.join(home, "Library", "Application Support", "Steam")];
		default:
			return [
				path.join(home, ".steam", "steam"),
				path.join(home, ".local", "share", "Steam"),
				path.join(home, ".var", "app", "com.valvesoftware.Steam", ".local", "share", "Steam"),
			];
	}
}

export function parseSteamLibraryFolders(text: string): string[] {
	return Array.from(text.matchAll(LIBRARY_PATH), (match) =>
		match[1].replace(/\\([\\"])/g, "$1"),
	);
}

// The engine reads `cwtools.cache.<game>` as the folder holding `common/`, which
// is `<install>/game` for the games that nest it. Browse and Steam discovery both
// go through here so they store the same shape.
export function vanillaContentRoot(game: GameDef, folder: string): string {
	const { vanillaSubdir } = game;
	return vanillaSubdir && path.basename(folder) !== vanillaSubdir
		? path.join(folder, vanillaSubdir)
		: folder;
}

const exists = (candidate: string): Promise<boolean> =>
	access(candidate).then(
		() => true,
		() => false,
	);

export async function findVanillaInstallations(
	game: GameDef,
	roots: readonly string[] = steamRoots(),
): Promise<string[]> {
	const found = new Set<string>();
	for (const root of roots) {
		const vdf = await readFile(
			path.join(root, "steamapps", "libraryfolders.vdf"),
			"utf8",
		).catch(() => "");
		for (const library of parseSteamLibraryFolders(vdf)) {
			const common = path.join(library, "steamapps", "common");
			const names = await readdir(common).catch(() => []);
			for (const name of names) {
				if (game.vanillaFolders.includes(name.toLowerCase())) {
					found.add(vanillaContentRoot(game, path.join(common, name)));
				}
			}
		}
	}
	const candidates = [...found];
	const present = await Promise.all(candidates.map(exists));
	return candidates.filter((_, index) => present[index]);
}
