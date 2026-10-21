import * as os from "os";
import * as path from "path";
import { existsSync, readFileSync, readdirSync } from "fs";
import type { GameDef } from "./games";

type VdfValue = string | VdfObject;
interface VdfObject {
	[key: string]: VdfValue;
}

// Steam's libraryfolders.vdf is KeyValues text. This small reader handles the
// quoted strings, braces, escaped backslashes/quotes and comments used by the
// file without depending on a third-party parser.
function tokenizeVdf(text: string): string[] {
	const tokens: string[] = [];
	for (let i = 0; i < text.length; ) {
		const ch = text[i];
		if (/\s/.test(ch)) {
			i++;
			continue;
		}
		if (ch === "/" && text[i + 1] === "/") {
			while (i < text.length && text[i] !== "\n") i++;
			continue;
		}
		if (ch === "{" || ch === "}") {
			tokens.push(ch);
			i++;
			continue;
		}
		if (ch !== '"') {
			// Bare tokens are accepted by KeyValues even though Steam writes
			// libraryfolders keys and paths quoted.
			const start = i;
			while (i < text.length && !/[\s{}]/.test(text[i])) i++;
			tokens.push(text.slice(start, i));
			continue;
		}
		i++;
		let value = "";
		while (i < text.length && text[i] !== '"') {
			if (text[i] === "\\" && i + 1 < text.length) {
				const next = text[i + 1];
				if (next === "\\" || next === '"') {
					value += next;
					i += 2;
					continue;
				}
			}
			value += text[i++];
		}
		if (i < text.length) i++;
		tokens.push(value);
	}
	return tokens;
}

export function parseSteamLibraryFolders(text: string): string[] {
	const tokens = tokenizeVdf(text);
	let cursor = 0;
	const parseObject = (): VdfObject => {
		const result: VdfObject = {};
		while (cursor < tokens.length && tokens[cursor] !== "}") {
			const key = tokens[cursor++];
			if (key === "{") continue;
			if (tokens[cursor] === "{") {
				cursor++;
				result[key] = parseObject();
				if (tokens[cursor] === "}") cursor++;
			} else if (cursor < tokens.length && tokens[cursor] !== "}") {
				result[key] = tokens[cursor++];
			}
		}
		return result;
	};
	const root = parseObject();
	const libraries = Object.entries(root).find(
		([key]) => key.toLowerCase() === "libraryfolders",
	)?.[1];
	if (!libraries || typeof libraries === "string") return [];
	const paths: string[] = [];
	for (const entry of Object.values(libraries)) {
		if (typeof entry === "string") {
			// Legacy libraryfolders.vdf stores each numbered entry as a path.
			paths.push(entry);
			continue;
		}
		const libraryPath = Object.entries(entry).find(
			([key]) => key.toLowerCase() === "path",
		)?.[1];
		if (typeof libraryPath === "string") paths.push(libraryPath);
	}
	return [...new Set(paths)];
}

export function steamRootCandidates(
	platform: NodeJS.Platform = process.platform,
	home: string = os.homedir(),
	env: NodeJS.ProcessEnv = process.env,
): string[] {
	if (platform === "win32") {
		const winPath = path.win32;
		return [
			env.STEAM_PATH,
			env.LOCALAPPDATA && winPath.join(env.LOCALAPPDATA, "Programs", "Steam"),
			env["ProgramFiles(x86)"] && winPath.join(env["ProgramFiles(x86)"], "Steam"),
			env.ProgramFiles && winPath.join(env.ProgramFiles, "Steam"),
		].filter((candidate): candidate is string => Boolean(candidate));
	}
	if (platform === "darwin") {
		return [path.join(home, "Library", "Application Support", "Steam")];
	}
	return [
		path.join(home, ".steam", "steam"),
		path.join(home, ".local", "share", "Steam"),
		"/usr/share/steam",
	];
}

export function findVanillaInstallations(
	game: GameDef,
	libraryFiles: Iterable<string>,
	exists: (candidate: string) => boolean = existsSync,
	directoryNames: (directory: string) => string[] = (directory) =>
		readdirSync(directory),
): string[] {
	const found = new Set<string>();
	for (const content of libraryFiles) {
		for (const library of parseSteamLibraryFolders(content)) {
			const pathApi = /^[A-Za-z]:[\\/]/.test(library) ? path.win32 : path;
			const common = pathApi.join(library, "steamapps", "common");
			let installedFolders: string[];
			try {
				installedFolders = directoryNames(common);
			} catch {
				installedFolders = [];
			}
			for (const folder of game.vanillaFolders) {
				const matchingFolder = installedFolders.find(
					(installed) => installed.toLowerCase() === folder.toLowerCase(),
				);
				const install = pathApi.join(common, matchingFolder ?? folder);
				const candidate = game.vanillaSubdir
					? pathApi.join(install, game.vanillaSubdir)
				: install;
				if ((matchingFolder !== undefined || exists(install)) && exists(candidate)) {
					found.add(candidate);
				}
			}
		}
	}
	return [...found];
}

export function discoverVanillaInstallations(
	game: GameDef,
	steamRoots = steamRootCandidates(),
): string[] {
	const contents: string[] = [];
	for (const root of steamRoots) {
		const file = path.join(root, "steamapps", "libraryfolders.vdf");
		try {
			if (existsSync(file)) contents.push(readFileSync(file, "utf8"));
		} catch {
			// An unreadable library is equivalent to one without known installs;
			// the command still offers the manual browse option.
		}
	}
	return findVanillaInstallations(game, contents);
}
