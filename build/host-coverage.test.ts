import * as assert from "node:assert";
import { existsSync, readFileSync, unlinkSync } from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { suite, test } from "vitest";
import {
	killProcessTree,
	runWithTimeout,
} from "./host-coverage";

const alive = (pid: number): boolean => {
	try {
		process.kill(pid, 0);
		return true;
	} catch {
		return false;
	}
};

suite("runWithTimeout", () => {
	test("resolves when the process exits 0", async () => {
		await runWithTimeout(
			"ok",
			process.execPath,
			["-e", "process.exit(0)"],
			{
				cwd: os.tmpdir(),
				timeoutMs: 5_000,
				graceMs: 100,
				stdio: "ignore",
			},
		);
	});

	test("rejects a non-zero exit", async () => {
		await assert.rejects(
			() =>
				runWithTimeout("fail", process.execPath, ["-e", "process.exit(2)"], {
					cwd: os.tmpdir(),
					timeoutMs: 5_000,
					graceMs: 100,
					stdio: "ignore",
				}),
			/fail failed with exit code 2/,
		);
	});

	test("kills a process that does not exit", async () => {
		const marker = path.join(
			os.tmpdir(),
			`cwtools-host-coverage-${process.pid}-${Date.now()}.pid`,
		);
		try {
			await assert.rejects(
				() =>
					runWithTimeout(
						"hang",
						process.execPath,
						[
							"-e",
							`require("node:fs").writeFileSync(${JSON.stringify(marker)}, String(process.pid)); setInterval(() => {}, 1000);`,
						],
						{
							cwd: os.tmpdir(),
							timeoutMs: 200,
							graceMs: 100,
							stdio: "ignore",
						},
					),
				/hang timed out after 200ms/,
			);
			if (existsSync(marker)) {
				const pid = Number(readFileSync(marker, "utf8"));
				assert.ok(Number.isInteger(pid) && pid > 0);
				assert.equal(alive(pid), false);
			}
		} finally {
			try {
				unlinkSync(marker);
			} catch {
				// the hang may have been killed before it wrote the marker
			}
		}
	});
});

suite("killProcessTree", () => {
	test("does not throw for a pid that is already gone", () => {
		assert.doesNotThrow(() => killProcessTree(1_000_000_007, "SIGTERM"));
	});
});
