/**
 * Shared output-channel logger for the extension host.
 *
 * Console output in VS Code extensions only appears in the developer tools
 * console, which end users never see.  This module writes to a dedicated
 * OutputChannel named "CWTools" so messages are visible in the Output panel
 * and persist across sessions.
 */
import { window } from "vscode";

const channel = window.createOutputChannel("CWTools");

// Adopted by the language client, so its server output lands here rather than
// in a second channel named after the client.
export const outputChannel = channel;

export function logInfo(message: string): void {
	channel.appendLine(message);
}

export function logWarn(message: string): void {
	channel.appendLine(`[WARN] ${message}`);
}

export function logError(message: string, err?: unknown): void {
	const suffix =
		err instanceof Error
			? `: ${err.message}`
			: err !== undefined && err !== null
				? `: ${String(err)}`
				: "";
	channel.appendLine(`[ERROR] ${message}${suffix}`);
}
