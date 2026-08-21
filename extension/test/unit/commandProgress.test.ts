import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";
import type { LanguageClient } from "vscode-languageclient/node";
import {
	LSPErrorCodes,
	WorkDoneProgressCancelNotification,
} from "vscode-languageserver-protocol";

const state = vi.hoisted(() => {
	class CancellationError extends Error {}

	const token = {
		isCancellationRequested: false,
		onCancellationRequested: (_listener: () => void) => ({
			dispose: () => undefined,
		}),
	};
	const requestType = {};
	const reported: Array<{ message?: string; increment?: number }> = [];
	const withProgress = vi.fn(
		(
			_options: unknown,
			task: (
				progress: { report: (v: { message?: string; increment?: number }) => void },
				token: unknown,
			) => Promise<unknown>,
		): Promise<unknown> =>
			task({ report: (v) => void reported.push(v) }, token),
	);

	return { CancellationError, requestType, token, withProgress, reported };
});

vi.mock("vscode", () => ({
	CancellationError: state.CancellationError,
	ProgressLocation: { Notification: 15 },
	window: { withProgress: state.withProgress },
}));

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: state.requestType },
}));

import {
	commandProgressActive,
	runCancellableExecuteCommand,
	serverSupportsCommandProgress,
} from "../../src/host/commandProgress";

/** A client whose server advertises `executeCommandProvider.workDoneProgress`. */
function progressCapableClient(overrides: Record<string, unknown> = {}) {
	return {
		initializeResult: {
			capabilities: { executeCommandProvider: { workDoneProgress: true } },
		},
		onProgress: vi.fn(() => ({ dispose: () => undefined })),
		sendNotification: vi.fn(() => Promise.resolve()),
		...overrides,
	};
}

suite("commandProgress", () => {
	beforeEach(() => {
		vi.clearAllMocks();
		state.token.isCancellationRequested = false;
		state.token.onCancellationRequested = () => ({
			dispose: () => undefined,
		});
		state.reported.length = 0;
	});

	suite("capability detection", () => {
		test("requires the server to advertise executeCommandProvider.workDoneProgress", () => {
			assert.strictEqual(
				serverSupportsCommandProgress(
					progressCapableClient() as unknown as LanguageClient,
				),
				true,
			);
			// An older server advertises the commands but not the flag.
			assert.strictEqual(
				serverSupportsCommandProgress({
					initializeResult: {
						capabilities: { executeCommandProvider: { commands: [] } },
					},
				} as unknown as LanguageClient),
				false,
			);
			// Nothing at all — the client hasn't finished starting.
			assert.strictEqual(
				serverSupportsCommandProgress({} as unknown as LanguageClient),
				false,
			);
		});
	});

	suite("token path (server drives progress)", () => {
		test("passes a workDoneToken and does not cancel the request itself", async () => {
			const sendRequest = vi.fn().mockResolvedValue("Workspace re-indexed.");
			const client = progressCapableClient({ sendRequest });

			const result = await runCancellableExecuteCommand(
				client as unknown as LanguageClient,
				"reindexWorkspace",
				[],
				"CWTools: Re-index workspace",
			);

			assert.strictEqual(result, "Workspace re-indexed.");
			const [, params, cancellation] = sendRequest.mock.calls[0] as [
				unknown,
				{ command: string; workDoneToken?: string },
				unknown,
			];
			assert.strictEqual(params.command, "reindexWorkspace");
			assert.ok(
				typeof params.workDoneToken === "string" &&
					params.workDoneToken.length > 0,
				"the request must carry a workDoneToken",
			);
			// Threading the VS Code token here would raise $/cancelRequest, which
			// tower-lsp answers by dropping the handler — killing the graceful
			// "cancelled" reply this path exists to receive.
			assert.strictEqual(cancellation, undefined);
		});

		test("gives each command a distinct token", async () => {
			const sendRequest = vi.fn().mockResolvedValue("done");
			const client = progressCapableClient({ sendRequest });
			const run = () =>
				runCancellableExecuteCommand(
					client as unknown as LanguageClient,
					"reindexWorkspace",
					[],
					"CWTools: Re-index workspace",
				);

			await run();
			await run();

			const tokens = sendRequest.mock.calls.map(
				(call) => (call[1] as { workDoneToken: string }).workDoneToken,
			);
			assert.strictEqual(
				new Set(tokens).size,
				2,
				`tokens must be unique per request, got ${tokens.join(", ")}`,
			);
		});

		test("turns server percentages into monotonic increments", async () => {
			let emit: ((value: unknown) => void) | undefined;
			const client = progressCapableClient({
				sendRequest: vi.fn().mockImplementation(() => {
					emit?.({ kind: "begin", title: "x", percentage: 0 });
					emit?.({ kind: "report", message: "Indexing…", percentage: 20 });
					emit?.({ kind: "report", message: "Validating…", percentage: 70 });
					// A late report from a finished phase must not rewind the bar.
					emit?.({ kind: "report", message: "stale", percentage: 30 });
					emit?.({ kind: "report", message: "no percentage" });
					emit?.({ kind: "end" });
					return Promise.resolve("done");
				}),
				onProgress: vi.fn((_type: unknown, _token: string, handler: unknown) => {
					emit = handler as (value: unknown) => void;
					return { dispose: () => undefined };
				}),
			});

			await runCancellableExecuteCommand(
				client as unknown as LanguageClient,
				"reindexWorkspace",
				[],
				"CWTools: Re-index workspace",
			);

			assert.deepStrictEqual(state.reported, [
				{ message: undefined, increment: 0 },
				{ message: "Indexing…", increment: 20 },
				{ message: "Validating…", increment: 50 },
				{ message: "stale", increment: 0 },
				{ message: "no percentage", increment: undefined },
			]);
		});

		test("cancel sends window/workDoneProgress/cancel for its own token", async () => {
			let fireCancel: (() => void) | undefined;
			state.token.onCancellationRequested = (listener: () => void) => {
				fireCancel = listener;
				return { dispose: () => undefined };
			};
			const sendRequest = vi.fn().mockImplementation(() => {
				fireCancel?.();
				return Promise.resolve("Re-index cancelled.");
			});
			const client = progressCapableClient({ sendRequest });

			const result = await runCancellableExecuteCommand(
				client as unknown as LanguageClient,
				"reindexWorkspace",
				[],
				"CWTools: Re-index workspace",
			);

			// The server returns normally with an honest message; cancelling is
			// not an error on this path.
			assert.strictEqual(result, "Re-index cancelled.");
			const sendNotification = client.sendNotification as ReturnType<typeof vi.fn>;
			const [type, params] = sendNotification.mock.calls[0] as [
				unknown,
				{ token: string },
			];
			assert.strictEqual(type, WorkDoneProgressCancelNotification.type);
			const sent = (
				sendRequest.mock.calls[0][1] as { workDoneToken: string }
			).workDoneToken;
			assert.strictEqual(
				params.token,
				sent,
				"cancel must name the token the request carried",
			);
		});

		test("serverProgress: false keeps the request-cancellation fallback", async () => {
			const sendRequest = vi.fn().mockResolvedValue([]);
			const client = progressCapableClient({ sendRequest });

			await runCancellableExecuteCommand(
				client as unknown as LanguageClient,
				"getGraphData",
				["focus", 3],
				"CWTools: Build graph",
				{ serverProgress: false },
			);

			const [, params, cancellation] = sendRequest.mock.calls[0] as [
				unknown,
				{ workDoneToken?: string },
				unknown,
			];
			assert.strictEqual(params.workDoneToken, undefined);
			assert.strictEqual(cancellation, state.token);
		});
	});

	suite("status-bar handover", () => {
		test("reports active only while a command notification is up", async () => {
			assert.strictEqual(commandProgressActive(), false);
			let duringCommand = false;
			const client = progressCapableClient({
				sendRequest: vi.fn().mockImplementation(() => {
					duringCommand = commandProgressActive();
					return Promise.resolve("done");
				}),
			});

			await runCancellableExecuteCommand(
				client as unknown as LanguageClient,
				"reindexWorkspace",
				[],
				"CWTools: Re-index workspace",
			);

			assert.strictEqual(duringCommand, true);
			assert.strictEqual(commandProgressActive(), false);
		});

		test("clears the active flag when the command fails", async () => {
			const client = progressCapableClient({
				sendRequest: vi.fn().mockRejectedValue(new Error("boom")),
			});

			await assert.rejects(() =>
				runCancellableExecuteCommand(
					client as unknown as LanguageClient,
					"reindexWorkspace",
					[],
					"CWTools: Re-index workspace",
				),
			);

			assert.strictEqual(commandProgressActive(), false);
		});
	});

	suite("fallback path (server without command progress)", () => {
		test("shows a cancellable notification and forwards its token", async () => {
			const sendRequest = vi.fn().mockResolvedValue("Workspace re-indexed.");

			const result = await runCancellableExecuteCommand(
				{ sendRequest } as unknown as LanguageClient,
				"reindexWorkspace",
				[],
				"CWTools: Re-index workspace",
			);

			assert.strictEqual(result, "Workspace re-indexed.");
			assert.deepStrictEqual(state.withProgress.mock.calls[0]?.[0], {
				location: 15,
				title: "CWTools: Re-index workspace",
				cancellable: true,
			});
			assert.deepStrictEqual(sendRequest.mock.calls, [
				[
					state.requestType,
					{ command: "reindexWorkspace", arguments: [] },
					state.token,
				],
			]);
		});

		test("normalizes a locally canceled request", async () => {
			state.token.isCancellationRequested = true;
			const sendRequest = vi
				.fn()
				.mockRejectedValue(new Error("Request canceled"));

			await assert.rejects(
				() =>
					runCancellableExecuteCommand(
						{ sendRequest } as unknown as LanguageClient,
						"reindexWorkspace",
						[],
						"CWTools: Re-index workspace",
					),
				(err: unknown) => err instanceof state.CancellationError,
			);
		});

		test("ignores a result that arrives after cancellation", async () => {
			const sendRequest = vi.fn().mockImplementation(() => {
				state.token.isCancellationRequested = true;
				return Promise.resolve("Workspace re-indexed.");
			});

			await assert.rejects(
				() =>
					runCancellableExecuteCommand(
						{ sendRequest } as unknown as LanguageClient,
						"reindexWorkspace",
						[],
						"CWTools: Re-index workspace",
					),
				(err: unknown) => err instanceof state.CancellationError,
			);
		});

		test("normalizes an LSP cancellation response", async () => {
			const sendRequest = vi
				.fn()
				.mockRejectedValue({ code: LSPErrorCodes.RequestCancelled });

			await assert.rejects(
				() =>
					runCancellableExecuteCommand(
						{ sendRequest } as unknown as LanguageClient,
						"reindexWorkspace",
						[],
						"CWTools: Re-index workspace",
					),
				(err: unknown) => err instanceof state.CancellationError,
			);
		});

		test("preserves a request failure when cancellation was not requested", async () => {
			const failure = new Error("Server unavailable");
			const sendRequest = vi.fn().mockRejectedValue(failure);

			await assert.rejects(
				() =>
					runCancellableExecuteCommand(
						{ sendRequest } as unknown as LanguageClient,
						"reindexWorkspace",
						[],
						"CWTools: Re-index workspace",
					),
				(err: unknown) => err === failure,
			);
		});
	});
});
