import * as assert from "assert";
import * as vscode from "vscode";
import sinon from "sinon";
import { activate, waitUntil, waitForServerReady, SAMPLE_ROOT } from "../support/utils";

suite("automatic server restart", function () {
	this.timeout(90_000);
	test("restores command availability after the server process crashes", async () => {
		const api = await activate();
		assert.ok(api);
		assert.ok(await waitForServerReady(api));
		const pid = api.serverProcessId();
		assert.ok(pid, "the running server must have a process");
		const sandbox = sinon.createSandbox();
		const execute = sandbox.stub(vscode.commands, "executeCommand").callThrough();
		try {
			process.kill(pid, "SIGKILL");
			assert.ok(await waitUntil(() => {
				const nextPid = api.serverProcessId();
				return nextPid !== undefined && nextPid !== pid && api.serverCommands().length > 0;
			}, 30_000), "client did not automatically start a new server");
			assert.ok(await waitForServerReady(api));
			for (const key of ["cwtoolsGraphAvailable", "cwtoolsFixAllAvailable", "cwtoolsFormatWorkspaceAvailable"]) {
				const values = execute.getCalls().filter((call) => call.args[0] === "setContext" && call.args[1] === key).map((call): unknown => call.args[2]);
				assert.deepStrictEqual(values, [false, true], `${key} must recover after the crash`);
			}
			// An actual LSP command must reach the replacement process too.
			const types = await vscode.commands.executeCommand("getFileTypes", vscode.Uri.file(SAMPLE_ROOT).toString());
			assert.ok(Array.isArray(types), "replacement server should answer getFileTypes");
		} finally {
			sandbox.restore();
		}
	});
});
