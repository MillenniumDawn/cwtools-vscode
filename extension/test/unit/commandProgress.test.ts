import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";
import { CancellationError } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";
import {
	LSPErrorCodes,
	WorkDoneProgressCancelNotification,
} from "vscode-languageserver-protocol";

const state = vi.hoisted(() => {
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

	return { requestType, token, withProgress, reported };
});

vi.mock("vscode", async (importOriginal) => ({
	...(await importOriginal<object>()),
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

type SendRequest = ReturnType<typeof vi.fn>;

/** A client whose server advertises `executeCommandProvider.workDoneProgress`. */
function progressCapableClient(sendRequest: SendRequest, onProgress?: unknown) {
	return {
		initializeResult: {
			capabilities: { executeCommandProvider: { workDoneProgress: true } },
		},
		onProgress: onProgress ?? (() => ({ dispose: () => undefined })),
		sendNotification: vi.fn(() => Promise.resolve()),
		sendRequest,
	};
}

const reindex = (client: object) =>
	runCancellableExecuteCommand(
		client as unknown as LanguageClient,
		"reindexWorkspace",
		[],
		"CWTools: Re-index workspace",
	);

const sentToken = (sendRequest: SendRequest, call = 0) =>
	(sendRequest.mock.calls[call][1] as { workDoneToken?: string }).workDoneToken;

suite("commandProgress", () => {
	beforeEach(() => {
		state.token.isCancellationRequested = false;
		state.token.onCancellationRequested = () => ({
			dispose: () => undefined,
		});
		state.reported.length = 0;
	});

	test("requires the server to advertise executeCommandProvider.workDoneProgress", () => {
		const supports = (client: object) =>
			serverSupportsCommandProgress(client as unknown as LanguageClient);
		assert.strictEqual(supports(progressCapableClient(vi.fn())), true);
		// An older server advertises the commands but not the flag.
		assert.strictEqual(
			supports({
				initializeResult: {
					capabilities: { executeCommandProvider: { commands: [] } },
				},
			}),
			false,
		);
		// Nothing at all: the client hasn't finished starting.
		assert.strictEqual(supports({}), false);
	});

	suite("token path (server drives progress)", () => {
		// Threading the VS Code token here would raise $/cancelRequest, which
		// tower-lsp answers by dropping the handler, killing the graceful
		// "cancelled" reply this path exists to receive.
		test("passes a distinct workDoneToken per request and does not cancel the request itself", async () => {
			const sendRequest = vi.fn().mockResolvedValue("Workspace re-indexed.");
			const client = progressCapableClient(sendRequest);

			assert.strictEqual(await reindex(client), "Workspace re-indexed.");
			await reindex(client);

			const [, params, cancellation] = sendRequest.mock.calls[0] as [
				unknown,
				{ command: string },
				unknown,
			];
			assert.strictEqual(params.command, "reindexWorkspace");
			assert.strictEqual(cancellation, undefined);
			assert.ok(sentToken(sendRequest, 0), "the request must carry a workDoneToken");
			assert.notStrictEqual(sentToken(sendRequest, 0), sentToken(sendRequest, 1));
		});

		test("turns server percentages into monotonic increments", async () => {
			let emit: ((value: unknown) => void) | undefined;
			const sendRequest = vi.fn().mockImplementation(() => {
				emit?.({ kind: "begin", title: "x", percentage: 0 });
				emit?.({ kind: "report", message: "Indexing…", percentage: 20 });
				emit?.({ kind: "report", message: "Validating…", percentage: 70 });
				// A late report from a finished phase must not rewind the bar.
				emit?.({ kind: "report", message: "stale", percentage: 30 });
				emit?.({ kind: "report", message: "no percentage" });
				emit?.({ kind: "end" });
				return Promise.resolve("done");
			});
			const onProgress = (_type: unknown, _token: string, handler: unknown) => {
				emit = handler as (value: unknown) => void;
				return { dispose: () => undefined };
			};

			await reindex(progressCapableClient(sendRequest, onProgress));

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
			const client = progressCapableClient(sendRequest);

			// The server returns normally with an honest message; cancelling is
			// not an error on this path.
			assert.strictEqual(await reindex(client), "Re-index cancelled.");
			assert.deepStrictEqual(client.sendNotification.mock.calls, [
				[
					WorkDoneProgressCancelNotification.type,
					{ token: sentToken(sendRequest) },
				],
			]);
		});

		test("serverProgress: false keeps the request-cancellation fallback", async () => {
			const sendRequest = vi.fn().mockResolvedValue([]);

			await runCancellableExecuteCommand(
				progressCapableClient(sendRequest) as unknown as LanguageClient,
				"getGraphData",
				["focus", 3],
				"CWTools: Build graph",
				{ serverProgress: false },
			);

			assert.strictEqual(sentToken(sendRequest), undefined);
			assert.strictEqual(sendRequest.mock.calls[0][2], state.token);
		});
	});

	test("reports active only while a command notification is up, including when it fails", async () => {
		assert.strictEqual(commandProgressActive(), false);
		let duringCommand = false;
		const sendRequest = vi.fn().mockImplementation(() => {
			duringCommand = commandProgressActive();
			return Promise.reject(new Error("boom"));
		});

		await assert.rejects(() => reindex(progressCapableClient(sendRequest)), /boom/);

		assert.strictEqual(duringCommand, true);
		assert.strictEqual(commandProgressActive(), false);
	});

	suite("fallback path (server without command progress)", () => {
		test("shows a cancellable notification and forwards its token", async () => {
			const sendRequest = vi.fn().mockResolvedValue("Workspace re-indexed.");

			assert.strictEqual(await reindex({ sendRequest }), "Workspace re-indexed.");

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

		test.each<[string, () => SendRequest]>([
			[
				"a locally canceled request",
				() => {
					state.token.isCancellationRequested = true;
					return vi.fn().mockRejectedValue(new Error("Request canceled"));
				},
			],
			[
				"a result that arrives after cancellation",
				() =>
					vi.fn().mockImplementation(() => {
						state.token.isCancellationRequested = true;
						return Promise.resolve("Workspace re-indexed.");
					}),
			],
			[
				"an LSP cancellation response",
				() => vi.fn().mockRejectedValue({ code: LSPErrorCodes.RequestCancelled }),
			],
		])("normalizes %s to a CancellationError", async (_name, makeSendRequest) => {
			await assert.rejects(
				() => reindex({ sendRequest: makeSendRequest() }),
				(err: unknown) => err instanceof CancellationError,
			);
		});

		test("preserves a request failure when cancellation was not requested", async () => {
			const failure = new Error("Server unavailable");

			await assert.rejects(
				() => reindex({ sendRequest: vi.fn().mockRejectedValue(failure) }),
				(err: unknown) => err === failure,
			);
		});
	});
});
