import * as os from 'os';
import * as path from 'path';
import { workspace, window, RelativePattern, Uri } from 'vscode';
import { existsSync as fsExistsSync } from 'fs';
import { detectFromFolder } from './engine';
import { existAndIsExe } from './executable';

const KNOWN_LANGUAGE_IDS = ["stellaris", "eu4", "hoi4", "ck2", "imperator", "vic2", "vic3", "ck3", "eu5"];

const GAMES = [
	{ id: "eu4", exeName: "eu4", binariesPrefix: false },
	{ id: "hoi4", exeName: "hoi4", binariesPrefix: false },
	{ id: "stellaris", exeName: "stellaris", binariesPrefix: false },
	{ id: "ck2", exeName: "CK2", binariesPrefix: false },
	{ id: "imperator", exeName: "imperator", binariesPrefix: true },
	{ id: "vic2", exeName: "v2game", binariesPrefix: false },
	{ id: "ck3", exeName: "ck3", binariesPrefix: true },
	{ id: "vic3", exeName: "victoria3", binariesPrefix: true },
	{ id: "eu5", exeName: "eu5", binariesPrefix: true },
];

async function findExeInFiles(gameExeName: string, binariesPrefix: boolean): Promise<Uri[]> {
	if (!workspace.workspaceFolders || workspace.workspaceFolders.length === 0) {
		return [];
	}

	const root = workspace.workspaceFolders[0];
	const isWin = os.platform() === "win32";
	const ext = isWin ? "*.exe" : "*";
	const prefix = binariesPrefix ? "binaries/" : "";
	const names = [gameExeName, gameExeName.toUpperCase(), gameExeName.toLowerCase()];
	const patterns = names.map(name => new RelativePattern(root, `${prefix}${name}${ext}`));

	const results = await Promise.all(patterns.map(p => workspace.findFiles(p)));
	const allFiles = results.flat();
	const validFiles = (await Promise.all(
		allFiles.map(async v => (await existAndIsExe(v.fsPath)) ? v : null)
	)).filter(Boolean) as Uri[];
	return validFiles;
}

async function detectLanguageId(): Promise<string | null> {
	const markerFiles = await workspace.findFiles("**/*.txt", "**/{.git,node_modules,out,dist}/**", 1);
	if (markerFiles.length === 1) {
		const doc = await workspace.openTextDocument(markerFiles[0]);
		if (KNOWN_LANGUAGE_IDS.includes(doc.languageId)) return doc.languageId;
	}
	if (workspace.workspaceFolders && workspace.workspaceFolders.length > 0) {
		return detectFromFolder(workspace.workspaceFolders[0].uri.fsPath, fsExistsSync);
	}
	return null;
}

export interface GameDetection {
	languageId: string;
	isVanillaFolder: boolean;
}

export async function detectGameAndVanilla(): Promise<GameDetection> {
	let guessedLanguageId: string | undefined | null = window.activeTextEditor?.document?.languageId;
	if (guessedLanguageId === undefined || !KNOWN_LANGUAGE_IDS.includes(guessedLanguageId)) {
		guessedLanguageId = await detectLanguageId();
	}

	let languageId = (guessedLanguageId && KNOWN_LANGUAGE_IDS.includes(guessedLanguageId)) ? guessedLanguageId : "paradox";

	const promises = GAMES.map(({ exeName, binariesPrefix }) =>
		findExeInFiles(exeName, binariesPrefix)
	);
	const results = await Promise.all(promises);

	let isVanillaFolder = false;
	for (let i = 0; i < results.length; i++) {
		const { id } = GAMES[i];
		if (results[i].length > 0 && (languageId === "paradox" || languageId === id)) {
			isVanillaFolder = true;
			languageId = id;
		}
	}

	if (
		workspace.workspaceFolders &&
		workspace.workspaceFolders.length > 0 &&
		path.basename(workspace.workspaceFolders[0].uri.fsPath) === "game"
	) {
		isVanillaFolder = true;
	}

	return { languageId, isVanillaFolder };
}
