/**
 * Shared output-channel logger for the extension host.
 *
 * Console output in VS Code extensions only appears in the developer tools
 * console, which end users never see.  This module writes to a dedicated
 * OutputChannel named "CWTools" so messages are visible in the Output panel
 * and persist across sessions.
 */
import { window } from "vscode";

const channel = window.createOutputChannel("CWTools", { log: true });

// Adopted by the language client, so its server output lands here rather than
// in a second channel named after the client.
export const outputChannel = channel;

export function logInfo(message: string): void {
	channel.appendLine(message);
}

export function logWarn(message: string): void {
	channel.appendLine(`[WARN] ${message}`);
}

// Best-effort human message for an unknown thrown value. The catch sites in
// the extension all want `Error ? .message : String(value)`, and logError wants
// the same but omitting the suffix entirely for undefined/null.
export function errorMessage(err: unknown): string {
	if (err instanceof Error) return err.message;
	if (err === undefined || err === null) return "";
	// Intentional: fall back to Object's default stringification ("[object
	// Object]") for unknown thrown values; covered by logger.test.ts.
	// eslint-disable-next-line @typescript-eslint/no-base-to-string
	return String(err);
}

export function logError(message: string, err?: unknown): void {
	const suffix = errorMessage(err);
	channel.appendLine(`[ERROR] ${message}${suffix ? `: ${suffix}` : ""}`);
}
