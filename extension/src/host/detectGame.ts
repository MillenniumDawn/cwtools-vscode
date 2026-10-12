import * as os from "os";
import type { Uri, WorkspaceFolder } from "vscode";
import { workspace, RelativePattern } from "vscode";
import { access } from "fs/promises";
import { detectFromFolder } from "./engine";
import { existAndIsExe } from "./executable";
import { GAMES } from "./games";

async function findExeInFiles(
	gameExeName: string,
	binariesPrefix: boolean,
	root: WorkspaceFolder,
): Promise<Uri[]> {
	const isWin = os.platform() === "win32";
	const ext = isWin ? "*.exe" : "*";
	const prefix = binariesPrefix ? "binaries/" : "";
	const names = [
		...new Set([
			gameExeName,
			gameExeName.toUpperCase(),
			gameExeName.toLowerCase(),
		]),
	];
	const namePattern = names.length === 1 ? names[0] : `{${names.join(",")}}`;
	const pattern = new RelativePattern(root, `${prefix}${namePattern}${ext}`);

	const allFiles = await workspace.findFiles(pattern);
	const validFiles = (
		await Promise.all(
			allFiles.map(async (v) => ((await existAndIsExe(v.fsPath)) ? v : null)),
		)
	).filter(Boolean) as Uri[];
	return validFiles;
}

async function detectLanguageId(root: WorkspaceFolder): Promise<string | null> {
	const exists = async (p: string): Promise<boolean> => {
		try {
			await access(p);
			return true;
		} catch {
			return false;
		}
	};
	return detectFromFolder(root.uri.fsPath, exists);
}

export interface GameDetection {
	languageId: string;
}

export async function detectGameAndVanilla(
	root: WorkspaceFolder,
): Promise<GameDetection> {
	const hintedLanguageId = await detectLanguageId(root);
	// Folder and content markers are enough to classify recognized workspaces.
	// Avoid a recursive executable search during activation; scan only when the
	// workspace gives us no game hint.
	if (hintedLanguageId) return { languageId: hintedLanguageId };

	const gamesToCheck = GAMES;
	const promises = gamesToCheck.map(({ exeName, binariesPrefix }) =>
		findExeInFiles(exeName, binariesPrefix, root),
	);
	const results = await Promise.all(promises);

	let languageId = "paradox";
	for (let i = 0; i < results.length; i++) {
		const { id } = gamesToCheck[i];
		if (results[i].length > 0) {
			languageId = id;
		}
	}

	return { languageId };
}
