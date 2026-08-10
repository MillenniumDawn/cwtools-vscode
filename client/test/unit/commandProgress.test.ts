import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";
import type { LanguageClient } from "vscode-languageclient/node";
import { LSPErrorCodes } from "vscode-languageserver-protocol";

const state = vi.hoisted(() => {
	class CancellationError extends Error {}

	const token = { isCancellationRequested: false };
	const requestType = {};
	const withProgress = vi.fn(
		(
			_options: unknown,
			task: (
				progress: unknown,
				token: { isCancellationRequested: boolean },
			) => Promise<unknown>,
		): Promise<unknown> => task({}, token),
	);

	return { CancellationError, requestType, token, withProgress };
});

vi.mock("vscode", () => ({
	CancellationError: state.CancellationError,
	ProgressLocation: { Notification: 15 },
	window: { withProgress: state.withProgress },
}));

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: state.requestType },
}));

import { runCancellableExecuteCommand } from "../../extension/commandProgress";

suite("commandProgress", () => {
	beforeEach(() => {
		vi.clearAllMocks();
		state.token.isCancellationRequested = false;
	});

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
		const sendRequest = vi.fn().mockRejectedValue(new Error("Request canceled"));

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
