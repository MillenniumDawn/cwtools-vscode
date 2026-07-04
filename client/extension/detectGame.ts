import * as os from 'os';
import * as path from 'path';
import type { Uri } from 'vscode';
import { workspace, RelativePattern } from 'vscode';
import { existsSync as fsExistsSync } from 'fs';
import { detectFromFolder } from './engine';
import { existAndIsExe } from './executable';
import { GAMES } from './games';

async function findExeInFiles(gameExeName: string, binariesPrefix: boolean): Promise<Uri[]> {
	if (!workspace.workspaceFolders || workspace.workspaceFolders.length === 0) {
		return [];
	}

	const root = workspace.workspaceFolders[0];
	const isWin = os.platform() === "win32";
	const ext = isWin ? "*.exe" : "*";
	const prefix = binariesPrefix ? "binaries/" : "";
	const names = [...new Set([gameExeName, gameExeName.toUpperCase(), gameExeName.toLowerCase()])];
	const namePattern = names.length === 1 ? names[0] : `{${names.join(',')}}`;
	const pattern = new RelativePattern(root, `${prefix}${namePattern}${ext}`);

	const allFiles = await workspace.findFiles(pattern);
	const validFiles = (await Promise.all(
		allFiles.map(async v => (await existAndIsExe(v.fsPath)) ? v : null)
	)).filter(Boolean) as Uri[];
	return validFiles;
}

async function detectLanguageId(): Promise<string | null> {
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
	let languageId = (await detectLanguageId()) ?? "paradox";

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
