import { CancellationError, ProgressLocation, window } from "vscode";
import { ExecuteCommandRequest } from "vscode-languageclient/node";
import type { LanguageClient } from "vscode-languageclient/node";
import { LSPErrorCodes } from "vscode-languageserver-protocol";

function isLspCancellation(err: unknown): boolean {
	if (typeof err !== "object" || err === null || !("code" in err)) {
		return false;
	}
	return (
		err.code === LSPErrorCodes.RequestCancelled ||
		err.code === LSPErrorCodes.ServerCancelled
	);
}

export async function runCancellableExecuteCommand(
	client: LanguageClient,
	command: string,
	args: unknown[],
	title: string,
): Promise<unknown> {
	const result: unknown = await window.withProgress<unknown>(
		{
			location: ProgressLocation.Notification,
			title,
			cancellable: true,
		},
		async (_progress, token) => {
			try {
				const result: unknown = await client.sendRequest(
					ExecuteCommandRequest.type,
					{ command, arguments: args },
					token,
				);
				if (token.isCancellationRequested) {
					throw new CancellationError();
				}
				return result;
			} catch (err) {
				if (token.isCancellationRequested || isLspCancellation(err)) {
					throw new CancellationError();
				}
				throw err;
			}
		},
	);
	return result;
}
