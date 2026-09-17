/**
 * Shared output-channel logger for the extension host.
 *
 * Console output in VS Code extensions only appears in the developer tools
 * console, which end users never see.  This module writes to a dedicated
 * OutputChannel named "CWTools" so messages are visible in the Output panel
 * and persist across sessions.
 */
import type { LogOutputChannel } from "vscode";

// Initialized by activation after the extension has confirmed that this
// workspace is enabled. The language client and commands then share this same
// channel instance.
export let outputChannel!: LogOutputChannel;

export function initializeLogger(channel: LogOutputChannel): void {
	outputChannel = channel;
}

function getOutputChannel(): LogOutputChannel {
	if (outputChannel === undefined) {
		throw new Error("CWTools logger has not been initialized");
	}
	return outputChannel;
}

export function logInfo(message: string): void {
	getOutputChannel().info(message);
}

export function logWarn(message: string): void {
	getOutputChannel().warn(message);
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
	getOutputChannel().error(`${message}${suffix ? `: ${suffix}` : ""}`);
}
