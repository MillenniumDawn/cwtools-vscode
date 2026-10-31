import * as assert from "assert";
import * as fs from "node:fs";
import * as vscode from "vscode";
import sinon from "sinon";
import { activate, waitUntil, waitForServerReady, SAMPLE_ROOT } from "../support/utils";

// Blocks the loop until the killed server is a zombie, so queued writes hit its
// dead pipe before the close event (#1160). The race fails unfixed code on some
// runs only. Linux only, like the host runner.
function crashWithQueuedWrites(pid: number): Promise<unknown> {
	const queued = Promise.allSettled(
		Array.from({ length: 64 }, () =>
			vscode.commands.executeCommand("getFileTypes", vscode.Uri.file(SAMPLE_ROOT).toString()),
		),
	);
	process.kill(pid, "SIGKILL");
	const deadline = Date.now() + 5_000;
	while (Date.now() < deadline && !/^\d+ \(.*\) Z/.test(fs.readFileSync(`/proc/${pid}/stat`, "utf8"))) {
		// busy-wait: nothing else may run until the server is gone
	}
	return queued;
}

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
			const queued = crashWithQueuedWrites(pid);
			assert.ok(await waitUntil(() => {
				const nextPid = api.serverProcessId();
				return nextPid !== undefined && nextPid !== pid && api.serverCommands().length > 0;
			}, 30_000), "client did not automatically start a new server");
			await queued;
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
					// initializeResult remains cached after a terminal close. Check the
					// user-facing command contexts instead: markStopped must hide them.
					for (const key of [
						"cwtoolsGraphAvailable",
						"cwtoolsFixAllAvailable",
						"cwtoolsFormatWorkspaceAvailable",
						"cwtoolsValidateWorkspaceAvailable",
					]) {
						const values = execute
							.getCalls()
							.filter(
								(call) =>
									call.args[0] === "setContext" && call.args[1] === key,
							)
							.map((call): unknown => call.args[2]);
						assert.strictEqual(
							values[values.length - 1],
							false,
							`${key} must be unavailable after the restart limit is reached`,
						);
					}
				} else {
					assert.ok(
						await waitUntil(() => {
							const nextPid = api.serverProcessId();
							return nextPid !== undefined && nextPid !== crashedPid && api.serverCommands().length > 0;
						}, 30_000),
						`client did not restart after crash ${crash}`,
					);
					assert.ok(
						await waitForServerReady(api),
						`server was still starting after crash ${crash}`,
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
