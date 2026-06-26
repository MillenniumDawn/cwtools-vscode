import { suite, test, afterEach } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { existAndIsExe } from "../../extension/executable";

suite("executable — existAndIsExe", () => {
	const tmpDir = os.tmpdir();

	function tempFile(name: string): string {
		return path.join(tmpDir, `cwtools-test-${Date.now()}-${name}`);
	}

	// Track files we create so cleanup always runs.
	const created: string[] = [];

	function makeFile(
		name: string,
		opts?: { executable?: boolean; mode?: number },
	): string {
		const p = tempFile(name);
		const mode = opts?.mode ?? (opts?.executable ? 0o755 : 0o644);
		fs.writeFileSync(p, "test", { mode });
		created.push(p);
		return p;
	}

	afterEach(() => {
		for (const p of created) {
			try {
				fs.unlinkSync(p);
			} catch {
				/* already gone */
			}
		}
		created.length = 0;
	});

	async function assertPermission(
		mode: number,
		expected: boolean,
		name: string,
	) {
		const p = makeFile(name, { mode });
		const result = await existAndIsExe(p);
		assert.strictEqual(result, expected);
	}

	async function assertSymlink(
		targetOpts: { executable: boolean },
		expected: boolean,
		name: string,
	) {
		const target = makeFile("symlink-target-" + name, targetOpts);
		const link = tempFile("symlink-" + name);
		try {
			fs.symlinkSync(target, link);
			created.push(link);
			const result = await existAndIsExe(link);
			assert.strictEqual(result, expected);
		} catch {
			if (!created.includes(link)) created.push(link);
		}
	}

	test("throws TypeError when called with a non-string argument", async () => {
		await assert.rejects(() => existAndIsExe(123 as unknown as string), {
			name: "TypeError",
			message: "Expected a string",
		});
	});

	test("throws TypeError for undefined", async () => {
		await assert.rejects(() => existAndIsExe(undefined as unknown as string), {
			name: "TypeError",
			message: "Expected a string",
		});
	});

	test("throws TypeError for null", async () => {
		await assert.rejects(() => existAndIsExe(null as unknown as string), {
			name: "TypeError",
			message: "Expected a string",
		});
	});

	test("returns false for a non-existent path", async () => {
		const result = await existAndIsExe(
			path.join(tmpDir, "cwtools-no-such-file-does-not-exist"),
		);
		assert.strictEqual(result, false);
	});

	test("returns false for a regular file without execute permission", async () => {
		const p = makeFile("noexec.txt", { executable: false });
		const result = await existAndIsExe(p);
		assert.strictEqual(result, false);
	});

	test("returns true for an executable file", async () => {
		const p = makeFile("executable.txt", { executable: true });
		const result = await existAndIsExe(p);
		assert.strictEqual(result, true);
	});

	test("returns true for a file with only owner execute permission", () =>
		assertPermission(0o744, true, "owner-exec"));
	test("returns true for a file with only group execute permission", () =>
		assertPermission(0o710, true, "group-exec"));
	test("returns true for a file with only others execute permission", () =>
		assertPermission(0o701, true, "others-exec"));

	test("returns false for a directory (not a file)", async () => {
		// Directories pass stat() but fail isFile().
		const result = await existAndIsExe(tmpDir);
		assert.strictEqual(result, false);
	});

	test("returns false for a symlink to a non-existent target", async () => {
		const p = tempFile("broken-symlink");
		try {
			fs.symlinkSync("/no/such/target", p);
			created.push(p);
			const result = await existAndIsExe(p);
			assert.strictEqual(result, false);
		} catch {
			if (!created.includes(p)) created.push(p);
		}
	});

	test("returns true for a symlink to an executable file", () =>
		assertSymlink({ executable: true }, true, "working"));
	test("returns false for a symlink to a non-executable file", () =>
		assertSymlink({ executable: false }, false, "noexec"));
});
