import { CancellationError, ProgressLocation, window } from "vscode";
import type { Disposable, Progress } from "vscode";
import { ExecuteCommandRequest } from "vscode-languageclient/node";
import type { LanguageClient } from "vscode-languageclient/node";
import {
	LSPErrorCodes,
	WorkDoneProgress,
	WorkDoneProgressCancelNotification,
} from "vscode-languageserver-protocol";
import type {
	WorkDoneProgressBegin,
	WorkDoneProgressEnd,
	WorkDoneProgressReport,
} from "vscode-languageserver-protocol";

type WorkDoneProgressValue =
	| WorkDoneProgressBegin
	| WorkDoneProgressReport
	| WorkDoneProgressEnd;

function isLspCancellation(err: unknown): boolean {
	if (typeof err !== "object" || err === null || !("code" in err)) {
		return false;
	}
	return (
		err.code === LSPErrorCodes.RequestCancelled ||
		err.code === LSPErrorCodes.ServerCancelled
	);
}

// Distinct per request for the life of the window. The server keys its cancel
// registry on the token, so reusing one across two in-flight commands would
// point Cancel at whichever registered last.
let nextTokenId = 0;
function newProgressToken(): string {
	nextTokenId += 1;
	return `cwtools/command/${nextTokenId}`;
}

/**
 * Whether the language server can report progress against a client-supplied
 * `workDoneToken` and stop work on `window/workDoneProgress/cancel`.
 *
 * Servers before that support still get a cancellable notification; it just
 * spins indeterminately and Cancel falls back to `$/cancelRequest`, which the
 * server can only act on between phases.
 */
export function serverSupportsCommandProgress(client: LanguageClient): boolean {
	return (
		client.initializeResult?.capabilities.executeCommandProvider
			?.workDoneProgress === true
	);
}

/**
 * Forward the server's `$/progress` for `token` into the notification the user
 * is looking at.
 *
 * VS Code's progress API takes an *increment*, while the server reports an
 * absolute percentage, so the difference is tracked here. A report with no
 * percentage (a phase with nothing countable in it) still updates the message
 * and leaves the bar where it is.
 */
function pipeServerProgress(
	client: LanguageClient,
	token: string,
	progress: Progress<{ message?: string; increment?: number }>,
): Disposable {
	let reported = 0;
	return client.onProgress(
		WorkDoneProgress.type,
		token,
		(value: WorkDoneProgressValue) => {
			if (value.kind === "end") {
				return;
			}
			const percentage =
				typeof value.percentage === "number" ? value.percentage : undefined;
			let increment: number | undefined;
			if (percentage !== undefined) {
				// Clamped at 0: the server's phases are monotonic, but a late
				// report from a phase that already ended would otherwise rewind
				// the bar, which VS Code renders as it jumping backwards.
				increment = Math.max(0, percentage - reported);
				reported = Math.max(reported, percentage);
			}
			progress.report({ message: value.message, increment });
		},
	);
}

// How many command notifications are currently on screen. The server drives
// both its status-bar `loadingBar` and this notification from the same phase
// updates, so while a command owns the notification the status-bar item is
// redundant — see `serverNotifications.ts`. A counter rather than a flag
// because `cacheVanilla` doesn't take the server's scan lock and can overlap
// another command.
let activeCommandProgress = 0;

/** Whether a command progress notification is currently on screen. */
export function commandProgressActive(): boolean {
	return activeCommandProgress > 0;
}

export interface CommandProgressOptions {
	/**
	 * Pass a `workDoneToken` so the server drives the bar and handles Cancel
	 * itself. Default `true`.
	 *
	 * `false` for commands the server runs to completion regardless
	 * (`getGraphData`): there, Cancel has to stay the `$/cancelRequest` path
	 * that drops the handler, because a button wired to a notification the
	 * server won't act on is a button that lies.
	 */
	serverProgress?: boolean;
}

/**
 * Run a server command inside a cancellable progress notification.
 *
 * Two cancellation paths, picked by whether the server supports command
 * progress (see {@link serverSupportsCommandProgress}):
 *
 * - **Token path.** The request carries a `workDoneToken`; Cancel sends
 *   `window/workDoneProgress/cancel` and then *waits for the server to return
 *   normally*, so the reply says what actually happened ("Re-index cancelled.",
 *   or "Caches cleared (…); re-index cancelled, rebuilding in the background").
 *   Deliberately no `$/cancelRequest` here: tower-lsp answers that by dropping
 *   the handler, which would kill the graceful reply this path exists for.
 * - **Fallback.** No token, and the VS Code cancellation token is threaded into
 *   `sendRequest` as before, so Cancel raises `$/cancelRequest` and this throws
 *   `CancellationError`.
 */
export async function runCancellableExecuteCommand(
	client: LanguageClient,
	command: string,
	args: unknown[],
	title: string,
	options: CommandProgressOptions = {},
): Promise<unknown> {
	const useToken =
		(options.serverProgress ?? true) && serverSupportsCommandProgress(client);
	const result: unknown = await window.withProgress<unknown>(
		{
			location: ProgressLocation.Notification,
			title,
			cancellable: true,
		},
		async (progress, token) => {
			activeCommandProgress += 1;
			const subscriptions: Disposable[] = [];
			const progressToken = useToken ? newProgressToken() : undefined;
			if (progressToken !== undefined) {
				subscriptions.push(
					pipeServerProgress(client, progressToken, progress),
					token.onCancellationRequested(() => {
						void client
							.sendNotification(WorkDoneProgressCancelNotification.type, {
								token: progressToken,
							})
							.catch(() => {
								// A server that already finished (or died) has nothing
								// to cancel; the request's own settlement is what ends
								// the notification either way.
							});
					}),
				);
			}
			try {
				const result: unknown = await client.sendRequest(
					ExecuteCommandRequest.type,
					{
						command,
						arguments: args,
						...(progressToken !== undefined
							? { workDoneToken: progressToken }
							: {}),
					},
					// Only the fallback cancels the request itself.
					progressToken === undefined ? token : undefined,
				);
				if (progressToken === undefined && token.isCancellationRequested) {
					throw new CancellationError();
				}
				return result;
			} catch (err) {
				if (
					progressToken === undefined &&
					(token.isCancellationRequested || isLspCancellation(err))
				) {
					throw new CancellationError();
				}
				throw err;
			} finally {
				activeCommandProgress -= 1;
				for (const subscription of subscriptions) {
					subscription.dispose();
				}
			}
		},
	);
	return result;
}
