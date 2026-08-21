import * as os from "os";
import * as path from "path";
import type { Uri } from "vscode";
import { l10n, window, workspace } from "vscode";
import { logWarn } from "./logger";

// Roots activation resolved: the extension's cache dir, the rules folder in
// use, and the configured base-game install. Workspace folders are read live,
// so one added after activation counts too.
let configuredRoots: readonly string[] = [];

export function setTrustedRoots(roots: readonly (string | undefined)[]): void {
	configuredRoots = roots.filter(
		(root): root is string => typeof root === "string" && root.trim() !== "",
	);
}

function trustedRoots(): string[] {
	return [
		...(workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath),
		...configuredRoots,
	];
}

// Lexical containment only: no stat, no symlink resolution, so a path is judged
// on what it says rather than on what the disk currently holds.
export function isTrustedPath(
	target: string,
	roots: readonly string[],
	platform: NodeJS.Platform = os.platform(),
): boolean {
	const p = platform === "win32" ? path.win32 : path.posix;
	if (!p.isAbsolute(target)) {
		return false;
	}
	return roots.some((root) => {
		if (!p.isAbsolute(root)) {
			return false;
		}
		const rel = p.relative(root, target);
		return (
			rel === "" ||
			(rel !== ".." && !rel.startsWith(`..${p.sep}`) && !p.isAbsolute(rel))
		);
	});
}

// The graph webview and the files tree both hand us locations the server
// produced. Open one straight away when it lands under a known root; anything
// else is a path we never scanned, so make the user say so first.
export async function confirmOpen(uri: Uri): Promise<boolean> {
	if (uri.scheme !== "file" || !path.isAbsolute(uri.fsPath)) {
		logWarn(`Refusing to open ${uri.toString()}: not an absolute file path.`);
		return false;
	}
	if (isTrustedPath(uri.fsPath, trustedRoots())) {
		return true;
	}
	const open = l10n.t("Open");
	const choice = await window.showWarningMessage(
		l10n.t(
			"CWTools: {0} is outside the workspace, the game install and the rules cache. Open it anyway?",
			uri.fsPath,
		),
		{ modal: true },
		open,
	);
	return choice === open;
}
