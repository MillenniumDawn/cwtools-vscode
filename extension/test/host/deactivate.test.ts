import * as assert from "assert";
import * as vscode from "vscode";
import sinon from "sinon";
import { activate } from "../support/utils";

// Last file in the labels that run it: it stops the shared language client, so
// any suite after it would find no server.
suite("deactivate", function () {
	this.timeout(60 * 1000);

	// VS Code exposes no way to deactivate a single extension, so the activation
	// API hands back the module's own deactivate(). That is the same function the
	// host calls when the window closes, not a second copy of it.
	test("stops the running client and tolerates a second call", async function () {
		const api = await activate();
		assert.ok(api, "activation API should be exposed");
		assert.ok(
			api.serverCommands().length > 0,
			"the client should be running before deactivate",
		);

		const sandbox = sinon.createSandbox();
		const executeCommand = sandbox
			.stub(vscode.commands, "executeCommand")
			.callThrough();
		const warning = sandbox.stub(vscode.window, "showWarningMessage");
		const error = sandbox.stub(vscode.window, "showErrorMessage");
		try {
			await api.deactivate();

			// Awaiting the thenable is what proves the handshake: stop() rejects when the
			// server does not answer shutdown and exit inside its 2s budget. It clears
			// initializeResult on the way in, so an empty command list is the client
			// having left Running.
			assert.deepStrictEqual(
				api.serverCommands(),
				[],
				"deactivate should have stopped the client",
			);
			assert.strictEqual(api.serverStatusText(), "CWTools: stopped");
			for (const key of [
				"cwtoolsGraphAvailable",
				"cwtoolsFixAllAvailable",
				"cwtoolsFormatWorkspaceAvailable",
			]) {
				assert.ok(
					executeCommand.calledWith("setContext", key, false),
					`deactivate should clear ${key}`,
				);
			}

			await vscode.commands.executeCommand("cwtools.formatWorkspace");
		} finally {
			sandbox.restore();
		}
		assert.ok(
			warning
				.getCalls()
				.some((call) =>
					String(call.args[0]).includes("language server is stopped"),
				),
			"commands against a stopped client should explain how to recover",
		);
		assert.strictEqual(error.called, false);

		// Disposing the client from context.subscriptions stops it again right after
		// deactivate() resolves, so a second call has to be harmless.
		await api.deactivate();
	});
});
