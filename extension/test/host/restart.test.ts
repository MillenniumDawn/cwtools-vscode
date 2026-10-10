import * as assert from "assert";
import * as vscode from "vscode";
import sinon from "sinon";
import { activate, waitUntil, waitForServerReady, SAMPLE_ROOT } from "../support/utils";

suite("automatic server restart", function () {
	this.timeout(180_000);
	test("restarts after a crash, then stops at the retry limit and allows manual recovery", async () => {
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

			// Four restarts are allowed in the three-minute window; the fifth
			// crash stops the client and updates its persistent status item.
			for (let crash = 2; crash <= 5; crash++) {
				const crashedPid = api.serverProcessId();
				assert.ok(crashedPid, `server process ${crash - 1} should be running`);
				process.kill(crashedPid, "SIGKILL");
				if (crash === 5) {
					assert.ok(
						await waitUntil(
							() => api.serverStatusText() === "CWTools: stopped",
							30_000,
						),
						"client did not report that the restart limit was reached",
					);
					// A normal restart briefly leaves Running as it replaces the
					// process; hold the terminal status long enough to distinguish it.
					await new Promise((resolve) => setTimeout(resolve, 1_000));
					assert.strictEqual(api.serverProcessId(), undefined);
					assert.deepStrictEqual(api.serverCommands(), []);
				} else {
					assert.ok(
						await waitUntil(() => {
							const nextPid = api.serverProcessId();
							return nextPid !== undefined && nextPid !== crashedPid && api.serverCommands().length > 0;
						}, 30_000),
						`client did not restart after crash ${crash}`,
					);
				}
			}

			// Restart from Stopped must call client.start(), not restart()'s
			// stop/start path (which throws unless the client is Running).
			await vscode.commands.executeCommand("cwtools.restartServer");
			assert.ok(
				await waitUntil(
					() => api.serverProcessId() !== undefined && api.serverStatusText() !== "CWTools: stopped",
					30_000,
				),
				"manual restart did not start a new server process",
			);
			assert.ok(await waitForServerReady(api));
		} finally {
			sandbox.restore();
		}
	});
});
