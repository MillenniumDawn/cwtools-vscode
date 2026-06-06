/**
 * Shared output-channel logger for the extension host.
 *
 * Console output in VS Code extensions only appears in the developer tools
 * console, which end users never see.  This module writes to a dedicated
 * OutputChannel named "CWTools" so messages are visible in the Output panel
 * and persist across sessions.
 */
import { window } from 'vscode';

const channel = window.createOutputChannel('CWTools');

export function logInfo(message: string): void {
	channel.appendLine(message);
}

export function logWarn(message: string): void {
	channel.appendLine(`[WARN] ${message}`);
}

export function logError(message: string, err?: unknown): void {
	const suffix = err instanceof Error ? `: ${err.message}` : err !== null ? `: ${String(err)}` : '';
	channel.appendLine(`[ERROR] ${message}${suffix}`);
}
